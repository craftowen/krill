//! 네트워크·CDN 없는 내장 PWA. gzip 파일은 소스 에셋의 결정적 사전 압축본이다.
use ring::digest;
use std::io::{self, Write};
use std::sync::OnceLock;
pub const APP: &[u8] = include_bytes!("assets/app.js");
const ASSETS: [(&str, &str, &[u8], &[u8]); 21] = [
    (
        "index.html",
        "text/html; charset=utf-8",
        include_bytes!("assets/index.html"),
        include_bytes!("assets/index.html.gz"),
    ),
    (
        "app.js",
        "text/javascript; charset=utf-8",
        APP,
        include_bytes!("assets/app.js.gz"),
    ),
    (
        "sw.js",
        "text/javascript; charset=utf-8",
        include_bytes!("assets/sw.js"),
        include_bytes!("assets/sw.js.gz"),
    ),
    (
        "crypto.js",
        "text/javascript; charset=utf-8",
        include_bytes!("assets/crypto.js"),
        include_bytes!("assets/crypto.js.gz"),
    ),
    (
        "i18n.js",
        "text/javascript; charset=utf-8",
        include_bytes!("assets/i18n.js"),
        include_bytes!("assets/i18n.js.gz"),
    ),
    (
        "client.js",
        "text/javascript; charset=utf-8",
        include_bytes!("assets/client.js"),
        include_bytes!("assets/client.js.gz"),
    ),
    (
        "terminal.js",
        "text/javascript; charset=utf-8",
        include_bytes!("assets/terminal.js"),
        include_bytes!("assets/terminal.js.gz"),
    ),
    (
        "qr.js",
        "text/javascript; charset=utf-8",
        include_bytes!("assets/qr.js"),
        include_bytes!("assets/qr.js.gz"),
    ),
    (
        "screens/home.js",
        "text/javascript; charset=utf-8",
        include_bytes!("assets/screens/home.js"),
        include_bytes!("assets/screens/home.js.gz"),
    ),
    (
        "screens/workspaces.js",
        "text/javascript; charset=utf-8",
        include_bytes!("assets/screens/workspaces.js"),
        include_bytes!("assets/screens/workspaces.js.gz"),
    ),
    (
        "screens/chat.js",
        "text/javascript; charset=utf-8",
        include_bytes!("assets/screens/chat.js"),
        include_bytes!("assets/screens/chat.js.gz"),
    ),
    (
        "screens/session.js",
        "text/javascript; charset=utf-8",
        include_bytes!("assets/screens/session.js"),
        include_bytes!("assets/screens/session.js.gz"),
    ),
    (
        "screens/details.js",
        "text/javascript; charset=utf-8",
        include_bytes!("assets/screens/details.js"),
        include_bytes!("assets/screens/details.js.gz"),
    ),
    (
        "screens/settings.js",
        "text/javascript; charset=utf-8",
        include_bytes!("assets/screens/settings.js"),
        include_bytes!("assets/screens/settings.js.gz"),
    ),
    (
        "app.css",
        "text/css; charset=utf-8",
        include_bytes!("assets/app.css"),
        include_bytes!("assets/app.css.gz"),
    ),
    (
        "manifest.json",
        "application/manifest+json",
        include_bytes!("assets/manifest.json"),
        include_bytes!("assets/manifest.json.gz"),
    ),
    (
        "icon.svg",
        "image/svg+xml",
        include_bytes!("assets/icon.svg"),
        include_bytes!("assets/icon.svg.gz"),
    ),
    (
        "icon-192.png",
        "image/png",
        include_bytes!("assets/icon-192.png"),
        include_bytes!("assets/icon-192.png.gz"),
    ),
    (
        "icon-512.png",
        "image/png",
        include_bytes!("assets/icon-512.png"),
        include_bytes!("assets/icon-512.png.gz"),
    ),
    (
        "icon-maskable-512.png",
        "image/png",
        include_bytes!("assets/icon-maskable-512.png"),
        include_bytes!("assets/icon-maskable-512.png.gz"),
    ),
    (
        "apple-touch-icon.png",
        "image/png",
        include_bytes!("assets/apple-touch-icon.png"),
        include_bytes!("assets/apple-touch-icon.png.gz"),
    ),
];
/// C1: 파일명·길이·원본 바이트로 식별하는 고정 출처 버전.
pub fn asset_hash() -> &'static str {
    static HASH: OnceLock<String> = OnceLock::new();
    HASH.get_or_init(|| {
        let mut files: Vec<_> = ASSETS
            .iter()
            .map(|(name, _, bytes, _)| (*name, *bytes))
            .collect();
        files.sort_unstable_by_key(|(name, _)| *name);
        let mut context = digest::Context::new(&digest::SHA256);
        for (name, bytes) in files {
            context.update(format!("{name}\n{}\n", bytes.len()).as_bytes());
            context.update(bytes);
        }
        super::devices::hex(context.finish().as_ref())
            .chars()
            .take(16)
            .collect()
    })
}
fn route(name: &str) -> String {
    if name == "index.html" {
        "/".into()
    } else {
        format!("/{name}")
    }
}
#[cfg(test)]
pub(super) fn serve(writer: &mut impl Write, path: &str, gzip: bool) -> io::Result<()> {
    serve_request(writer, path, "", gzip)
}
pub(super) fn serve_request(
    writer: &mut impl Write,
    path: &str,
    _query: &str,
    gzip: bool,
) -> io::Result<()> {
    let asset = ASSETS.iter().find(|(name, ..)| route(name) == path);
    let (status, mime, body, encoding) = asset.map_or(
        (
            "404 Not Found",
            "text/plain; charset=utf-8",
            b"Not found".as_slice(),
            "",
        ),
        |(_, mime, plain, compressed)| {
            (
                "200 OK",
                *mime,
                if gzip { *compressed } else { *plain },
                if gzip {
                    "Content-Encoding: gzip\r\n"
                } else {
                    ""
                },
            )
        },
    );
    write!(
        writer,
        "HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\n{encoding}Cache-Control: no-store\r\nVary: Accept-Encoding\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'self'; connect-src 'self' ws: wss:; img-src 'self' blob: data:; script-src 'self'; style-src 'self'; base-uri 'none'; frame-ancestors 'none'\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    writer.write_all(body)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn embedded_routes_and_gzip_budget() {
        assert!(
            ASSETS
                .iter()
                .map(|(_, _, _, compressed)| compressed.len())
                .sum::<usize>()
                <= 150 * 1024
        );
        for (name, mime, plain, _) in ASSETS {
            let route = route(name);
            let mut response = Vec::new();
            serve(&mut response, &route, false).unwrap();
            assert!(response.starts_with(b"HTTP/1.1 200 OK"));
            assert!(String::from_utf8_lossy(&response).contains(mime));
            assert!(response.ends_with(plain));
            let mut response = Vec::new();
            serve(&mut response, &route, true).unwrap();
            assert!(String::from_utf8_lossy(&response).contains("Content-Encoding: gzip"));
        }
        let js = String::from_utf8_lossy(APP);
        assert!(!js.contains("crypto.subtle"));
        assert!(!js.contains("crypto.randomUUID"));
        assert!(include_str!("assets/client.js").contains("getRandomValues"));
        assert!(!js.contains("innerHTML"));
    }
}

#[cfg(test)]
mod hash_tests {
    use super::*;
    #[test]
    fn disk_hash_and_source_file_inventory_match() {
        let source = include_str!("pwa.rs");
        let marker = ["include_bytes!", "(\"assets/"].concat();
        let listed: std::collections::BTreeSet<_> = source
            .split(&marker)
            .skip(1)
            .filter_map(|s| s.split('"').next())
            .filter(|n| std::path::Path::new(n).extension() != Some(std::ffi::OsStr::new("gz")))
            .collect();
        let embedded: std::collections::BTreeSet<_> = ASSETS.iter().map(|a| a.0).collect();
        assert_eq!(listed, embedded);
        let mut context = digest::Context::new(&digest::SHA256);
        for name in embedded {
            let bytes = std::fs::read(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("src/mobile/assets")
                    .join(name),
            )
            .unwrap();
            context.update(format!("{name}\n{}\n", bytes.len()).as_bytes());
            context.update(&bytes);
        }
        assert_eq!(
            asset_hash(),
            super::super::devices::hex(context.finish().as_ref())
                .chars()
                .take(16)
                .collect::<String>()
        );
    }
}
