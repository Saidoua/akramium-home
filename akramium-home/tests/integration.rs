//! The daemon over real HTTP: a server on a free port, a blocking client, the same requests
//! a browser, curl, or a WebDAV client would send. Each test gets its own data directory.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};

struct Server {
    base: String,
    host: String,
    dir: tempfile::TempDir,
}

impl Server {
    /// Starts the daemon on 127.0.0.1:0 in a background thread with its own runtime.
    fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut config = home_core::Config::default();
        config.data_dir = dir.path().join("data");
        config.listen = "127.0.0.1:0".parse().unwrap();
        let (tx, rx) = std::sync::mpsc::channel::<SocketAddr>();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
            runtime.block_on(async move {
                let app = akramium_home::build(config).await.unwrap();
                let listener = tokio::net::TcpListener::bind(app.listen).await.unwrap();
                tx.send(listener.local_addr().unwrap()).unwrap();
                axum::serve(listener, app.router.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap();
            });
        });
        let address = rx.recv().unwrap();
        Server { base: format!("http://{address}"), host: address.to_string(), dir }
    }

    fn setup_token(&self) -> String {
        let link = std::fs::read_to_string(self.dir.path().join("data/setup-link")).unwrap();
        link.trim().rsplit("token=").next().unwrap().to_string()
    }
}

struct Reply {
    status: u16,
    headers: http::HeaderMap,
    body: Vec<u8>,
}

impl Reply {
    fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| panic!("not JSON ({e}): {}", self.text()))
    }
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    fn header(&self, name: &str) -> &str {
        self.headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or("")
    }
}

/// A deliberately plain HTTP/1.1 client: one connection per request, any method (WebDAV's
/// included), no redirects followed, nothing added that the test did not ask for.
struct Client<'a> {
    server: &'a Server,
    cookie: Option<String>,
}

fn decode_chunked(mut raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let line_end = raw.windows(2).position(|w| w == b"\r\n").expect("chunk size line");
        let size = usize::from_str_radix(std::str::from_utf8(&raw[..line_end]).unwrap().split(';').next().unwrap().trim(), 16).unwrap();
        raw = &raw[line_end + 2..];
        if size == 0 {
            return out;
        }
        out.extend_from_slice(&raw[..size]);
        raw = &raw[size + 2..];
    }
}

impl<'a> Client<'a> {
    fn new(server: &'a Server) -> Self {
        Client { server, cookie: None }
    }

    fn send(&self, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> Reply {
        let mut head = format!("{method} {path} HTTP/1.1\r\nconnection: close\r\ncontent-length: {}\r\n", body.len());
        if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
            head.push_str(&format!("host: {}\r\n", self.server.host));
        }
        if let Some(c) = &self.cookie {
            head.push_str(&format!("cookie: {c}\r\n"));
        }
        for (k, v) in headers {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str("\r\n");

        let mut stream = TcpStream::connect(&self.server.host).unwrap();
        stream.set_read_timeout(Some(std::time::Duration::from_secs(60))).unwrap();
        stream.write_all(head.as_bytes()).unwrap();
        stream.write_all(body).unwrap_or_else(|e| panic!("{method} {path}: the server stopped reading the body: {e}"));
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).unwrap_or_else(|e| panic!("{method} {path}: {e}"));

        let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap_or_else(|| panic!("{method} {path}: no response head"));
        let head_text = String::from_utf8_lossy(&raw[..split]).into_owned();
        let mut lines = head_text.split("\r\n");
        let status: u16 = lines.next().unwrap().split(' ').nth(1).unwrap().parse().unwrap();
        let mut map = http::HeaderMap::new();
        for line in lines {
            if let Some((k, v)) = line.split_once(':') {
                map.append(http::HeaderName::from_bytes(k.trim().as_bytes()).unwrap(), v.trim().parse().unwrap());
            }
        }
        let payload = &raw[split + 4..];
        let chunked = map.get("transfer-encoding").is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"chunked"));
        Reply { status, headers: map, body: if chunked { decode_chunked(payload) } else { payload.to_vec() } }
    }

    fn get(&self, path: &str) -> Reply {
        self.send("GET", path, &[], b"")
    }

    fn post_json(&self, path: &str, body: serde_json::Value) -> Reply {
        self.send("POST", path, &[("content-type", "application/json")], body.to_string().as_bytes())
    }

    fn sign_in(&mut self, name: &str, password: &str) -> Reply {
        let reply = self.post_json("/api/login", serde_json::json!({ "name": name, "password": password }));
        self.keep_cookie(&reply);
        reply
    }

    fn keep_cookie(&mut self, reply: &Reply) {
        if let Some(c) = reply.headers.get("set-cookie").and_then(|v| v.to_str().ok()) {
            self.cookie = Some(c.split(';').next().unwrap().to_string());
        }
    }

    /// First-run setup as `alice`; leaves the client signed in.
    fn set_up(&mut self) {
        let token = self.server.setup_token();
        let reply = self.post_json("/api/setup", serde_json::json!({ "token": token, "name": "alice", "password": "correct horse" }));
        assert_eq!(reply.status, 200, "{}", reply.text());
        self.keep_cookie(&reply);
    }
}

const BASIC_ALICE: &str = "Basic YWxpY2U6Y29ycmVjdCBob3JzZQ=="; // alice:correct horse

fn pseudo_random(len: usize) -> Vec<u8> {
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect()
}

#[test]
fn first_run_setup_happens_once() {
    let server = Server::start();
    let mut client = Client::new(&server);
    let token = server.setup_token();

    assert_eq!(client.get("/").header("location"), "/setup");
    assert_eq!(client.get("/setup?token=wrong").status, 403);
    assert_eq!(client.get(&format!("/setup?token={token}")).status, 200);
    let weak = client.post_json("/api/setup", serde_json::json!({ "token": token, "name": "alice", "password": "short" }));
    assert_eq!(weak.status, 400, "a rejected password must not burn the link");
    client.set_up();

    assert_eq!(client.get(&format!("/setup?token={token}")).status, 410);
    let again = client.post_json("/api/setup", serde_json::json!({ "token": token, "name": "mallory", "password": "another pass" }));
    assert_eq!(again.status, 410);
    assert!(!server.dir.path().join("data/setup-link").exists(), "the link file goes once it is used");
    assert_eq!(client.get("/api/me").json()["name"], "alice");
    assert_eq!(client.get("/").header("location"), "/drive/");
    assert_eq!(Client::new(&server).get("/api/me").status, 401);
    assert_eq!(Client::new(&server).get("/drive/").header("location"), "/login?next=/drive/");
}

#[test]
fn chunked_upload_range_download_and_trash() {
    let server = Server::start();
    let mut client = Client::new(&server);
    client.set_up();

    let folder = client.post_json("/api/drive/folders", serde_json::json!({ "name": "Videos" })).json();
    let folder_id = folder["id"].as_i64().unwrap();
    let data = pseudo_random(20 * 1024 * 1024);
    let begun = client.post_json("/api/drive/uploads", serde_json::json!({ "name": "film.bin", "parent": folder_id, "size": data.len() })).json();
    let id = begun["id"].as_str().unwrap().to_string();
    let chunk = begun["chunk"].as_u64().unwrap() as usize;

    let mut offset = 0;
    let mut resent = false;
    while offset < data.len() {
        let end = (offset + chunk).min(data.len());
        let reply = client.send("PUT", &format!("/api/drive/uploads/{id}/{offset}"), &[], &data[offset..end]);
        assert_eq!(reply.status, 200, "{}", reply.text());
        if !resent {
            // The same chunk again, as after a lost reply: refused, and the count is unchanged.
            resent = true;
            assert_eq!(client.send("PUT", &format!("/api/drive/uploads/{id}/{offset}"), &[], &data[offset..end]).status, 409);
            assert_eq!(client.get(&format!("/api/drive/uploads/{id}")).json()["received"], end);
        }
        offset = end;
    }
    let file = client.post_json(&format!("/api/drive/uploads/{id}/finish"), serde_json::json!({})).json();
    let file_id = file["id"].as_i64().unwrap();
    assert_eq!(file["size"], data.len());

    let whole = client.get(&format!("/api/drive/files/{file_id}/content"));
    assert_eq!(whole.status, 200);
    assert!(whole.body == data, "the downloaded bytes are the uploaded bytes");
    let part = client.send("GET", &format!("/api/drive/files/{file_id}/content"), &[("range", "bytes=1048576-1048585")], b"");
    assert_eq!(part.status, 206);
    assert_eq!(part.body, &data[1_048_576..=1_048_585]);
    assert_eq!(part.header("content-range"), format!("bytes 1048576-1048585/{}", data.len()));

    // Uploaded HTML never renders.
    let page = client.send("PUT", "/api/drive/files?name=page.html", &[], b"<script>alert(1)</script>").json();
    let served = client.get(&format!("/api/drive/files/{}/content", page["id"]));
    assert!(served.header("content-disposition").starts_with("attachment"));
    assert!(served.header("content-security-policy").starts_with("sandbox"));
    assert_eq!(served.header("x-content-type-options"), "nosniff");

    assert_eq!(client.send("PUT", "/api/drive/files?name=..%2Fescape", &[], b"x").status, 400);
    assert_eq!(client.post_json("/api/drive/folders", serde_json::json!({ "name": ".." })).status, 400);

    // The folder goes to the trash in one piece and comes back whole.
    assert_eq!(client.send("DELETE", &format!("/api/drive/files/{folder_id}"), &[], b"").status, 204);
    assert_eq!(client.get(&format!("/api/drive/files/{file_id}/content")).status, 404);
    assert_eq!(client.get("/api/drive/trash").json().as_array().unwrap().len(), 1);
    assert_eq!(client.post_json(&format!("/api/drive/trash/{folder_id}/restore"), serde_json::json!({})).status, 200);
    assert_eq!(client.get(&format!("/api/drive/files/{file_id}/content")).status, 200);
}

#[test]
fn share_links_open_without_an_account_and_expire() {
    let server = Server::start();
    let mut owner = Client::new(&server);
    owner.set_up();
    let album = owner.post_json("/api/drive/folders", serde_json::json!({ "name": "Album" })).json();
    let inside = owner.send("PUT", &format!("/api/drive/files?name=note.txt&parent={}", album["id"]), &[], b"hello <b>world</b>").json();
    let outside = owner.send("PUT", "/api/drive/files?name=secret.txt", &[], b"secret").json();

    let share = owner.post_json(&format!("/api/drive/files/{}/shares", album["id"]), serde_json::json!({ "expires_in": 2 })).json();
    let token = share["token"].as_str().unwrap();
    let visitor = Client::new(&server);

    let page = visitor.get(&format!("/s/{token}"));
    assert_eq!(page.status, 200);
    assert!(page.text().contains("note.txt"));
    assert_eq!(page.header("x-robots-tag"), "noindex, nofollow");
    assert_eq!(page.header("referrer-policy"), "no-referrer");
    let item = visitor.get(&format!("/s/{token}/i/{}", inside["id"]));
    assert!(item.text().contains("hello &lt;b&gt;world&lt;/b&gt;"), "text is shown escaped");
    assert_eq!(visitor.get(&format!("/s/{token}/content/{}", inside["id"])).body, b"hello <b>world</b>");
    assert_eq!(visitor.get(&format!("/s/{token}/content/{}", outside["id"])).status, 404, "nothing outside the shared folder");
    assert_eq!(visitor.get(&format!("/s/{token}/i/{}", outside["id"])).status, 404);
    assert_eq!(visitor.get("/s/not-a-token").status, 404);
    assert_eq!(visitor.get("/api/drive/files").status, 401, "a link is not a session");
    assert_eq!(visitor.send("DELETE", &format!("/api/drive/files/{}", inside["id"]), &[], b"").status, 401);

    std::thread::sleep(std::time::Duration::from_millis(2300));
    assert_eq!(visitor.get(&format!("/s/{token}")).status, 410);
    assert_eq!(visitor.get(&format!("/s/{token}/content/{}", inside["id"])).status, 410);

    let forever = owner.post_json(&format!("/api/drive/files/{}/shares", inside["id"]), serde_json::json!({})).json();
    let token = forever["token"].as_str().unwrap();
    assert_eq!(visitor.get(&format!("/s/{token}")).status, 200);
    assert_eq!(owner.send("DELETE", &format!("/api/drive/shares/{}", forever["id"]), &[], b"").status, 204);
    assert_eq!(visitor.get(&format!("/s/{token}")).status, 404);
}

#[test]
fn webdav_shares_the_store_and_keeps_its_own_door() {
    let server = Server::start();
    let mut web = Client::new(&server);
    web.set_up();
    let dav = Client::new(&server);
    let auth = [("authorization", BASIC_ALICE)];

    assert_eq!(dav.send("PROPFIND", "/dav/", &[], b"").status, 401);
    assert!(dav.send("PROPFIND", "/dav/", &[], b"").header("www-authenticate").starts_with("Basic"));
    assert_eq!(web.send("PROPFIND", "/dav/", &[], b"").status, 401, "the session cookie is not accepted on /dav");
    assert_eq!(dav.send("GET", "/api/drive/files", &auth, b"").status, 401, "a name and password are not accepted on the API");

    assert_eq!(dav.send("MKCOL", "/dav/Docs", &auth, b"").status, 201);
    assert_eq!(dav.send("PUT", "/dav/Docs/a%20file.txt", &auth, b"first").status, 201);
    assert_eq!(dav.send("PUT", "/dav/Docs/a%20file.txt", &auth, b"second version").status, 204);
    assert_eq!(dav.send("GET", "/dav/Docs/a%20file.txt", &auth, b"").body, b"second version");
    let listing = dav.send("PROPFIND", "/dav/Docs/", &[("authorization", BASIC_ALICE), ("depth", "1")], b"");
    assert_eq!(listing.status, 207);
    assert!(listing.text().contains("/dav/Docs/a%20file.txt"), "{}", listing.text());

    // The page's API sees what WebDAV wrote, with the right size and type.
    let root = web.get("/api/drive/files").json();
    let docs = root["entries"].as_array().unwrap().iter().find(|e| e["name"] == "Docs").unwrap()["id"].as_i64().unwrap();
    let inside = web.get(&format!("/api/drive/files?folder={docs}")).json();
    assert_eq!(inside["entries"][0]["name"], "a file.txt");
    assert_eq!(inside["entries"][0]["size"], 14);

    let destination = format!("{}/dav/renamed.md", server.base);
    assert_eq!(dav.send("MOVE", "/dav/Docs/a%20file.txt", &[("authorization", BASIC_ALICE), ("destination", &destination)], b"").status, 201);
    let root = web.get("/api/drive/files").json();
    let moved = root["entries"].as_array().unwrap().iter().find(|e| e["name"] == "renamed.md").unwrap().clone();
    assert_eq!(moved["mime"], "text/markdown", "the type follows the new name");

    assert_eq!(dav.send("PUT", "/dav/..%2F..%2Fescape.txt", &auth, b"x").status, 400);
    assert_eq!(dav.send("PUT", "/dav/._renamed.md", &auth, b"apple double").status, 201);
    let names: Vec<String> = web.get("/api/drive/files").json()["entries"].as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap().to_string()).collect();
    assert!(!names.iter().any(|n| n.starts_with("._")), "client droppings stay out of the page: {names:?}");

    assert_eq!(dav.send("DELETE", "/dav/Docs/", &auth, b"").status, 204);
    assert_eq!(dav.send("DELETE", "/dav/._renamed.md", &auth, b"").status, 204);
    let trash: Vec<String> = web.get("/api/drive/trash").json().as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(trash, ["Docs"], "a folder is one trash item; droppings skip the trash");
}

#[test]
fn guards_refuse_unknown_hosts_and_cross_site_writes() {
    let server = Server::start();
    let mut client = Client::new(&server);
    client.set_up();

    assert_eq!(client.send("GET", "/login", &[("host", "evil.example")], b"").status, 421);
    assert_eq!(client.send("GET", "/login", &[("host", "127.0.0.1.evil.example")], b"").status, 421);
    assert_eq!(client.send("GET", "/login", &[("host", "akramium.local:11720")], b"").status, 200);

    let body = br#"{"name":"X"}"#;
    let json = ("content-type", "application/json");
    assert_eq!(client.send("POST", "/api/drive/folders", &[json, ("origin", "http://evil.example")], body).status, 403);
    assert_eq!(client.send("POST", "/api/drive/folders", &[json, ("origin", "null")], body).status, 403);
    assert_eq!(client.send("POST", "/api/drive/folders", &[json, ("sec-fetch-site", "cross-site")], body).status, 403);
    let own = format!("http://{}", server.host);
    assert_eq!(client.send("POST", "/api/drive/folders", &[json, ("origin", &own), ("sec-fetch-site", "same-origin")], body).status, 201);
    assert_eq!(client.send("GET", "/api/drive/files", &[("origin", "http://evil.example")], b"").status, 200, "reads are not writes");
    assert_eq!(client.get("/api/drive/files").json()["entries"].as_array().unwrap().len(), 1, "the refused writes changed nothing");
}

#[test]
fn wrong_passwords_lock_the_address_out_and_people_stay_apart() {
    let server = Server::start();
    let mut admin = Client::new(&server);
    admin.set_up();
    let mine = admin.send("PUT", "/api/drive/files?name=mine.txt", &[], b"private").json();
    assert_eq!(admin.post_json("/api/users", serde_json::json!({ "name": "bob", "password": "bobs password" })).status, 201);

    let mut bob = Client::new(&server);
    assert_eq!(bob.sign_in("bob", "bobs password").status, 200);
    assert_eq!(bob.get("/api/users").status, 403, "only admins manage people");
    assert_eq!(bob.get(&format!("/api/drive/files/{}/content", mine["id"])).status, 404, "one person cannot read another's files");
    assert_eq!(bob.get("/api/drive/files").json()["entries"].as_array().unwrap().len(), 0);

    let mut guesser = Client::new(&server);
    for _ in 0..5 {
        assert_eq!(guesser.sign_in("alice", "not the password").status, 400);
    }
    let locked = guesser.sign_in("alice", "correct horse");
    assert_eq!(locked.status, 429, "even the right password waits now");
    assert!(locked.header("retry-after").parse::<u64>().unwrap() > 0);
    assert_eq!(admin.get("/api/me").status, 200, "people already signed in are not thrown out");
}
