//! 네트워크·CDN 없는 내장 PWA. gzip 파일은 소스 에셋의 결정적 사전 압축본이다.
use std::io::{self, Write};
pub const APP: &[u8] = include_bytes!("assets/app.js");
const ASSETS: [(&str, &str, &[u8], &[u8]); 5] = [
    (
        "/",
        "text/html; charset=utf-8",
        include_bytes!("assets/index.html"),
        include_bytes!("assets/index.html.gz"),
    ),
    (
        "/app.js",
        "text/javascript; charset=utf-8",
        APP,
        include_bytes!("assets/app.js.gz"),
    ),
    (
        "/app.css",
        "text/css; charset=utf-8",
        include_bytes!("assets/app.css"),
        include_bytes!("assets/app.css.gz"),
    ),
    (
        "/manifest.json",
        "application/manifest+json",
        include_bytes!("assets/manifest.json"),
        include_bytes!("assets/manifest.json.gz"),
    ),
    (
        "/icon.svg",
        "image/svg+xml",
        include_bytes!("assets/icon.svg"),
        include_bytes!("assets/icon.svg.gz"),
    ),
];
pub(super) fn serve(writer: &mut impl Write, path: &str, gzip: bool) -> io::Result<()> {
    let asset = ASSETS.iter().find(|(route, ..)| *route == path);
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
        for (route, mime, plain, _) in ASSETS {
            let mut response = Vec::new();
            serve(&mut response, route, false).unwrap();
            assert!(response.starts_with(b"HTTP/1.1 200 OK"));
            assert!(String::from_utf8_lossy(&response).contains(mime));
            assert!(response.ends_with(plain));
            let mut response = Vec::new();
            serve(&mut response, route, true).unwrap();
            assert!(String::from_utf8_lossy(&response).contains("Content-Encoding: gzip"));
        }
        let js = String::from_utf8_lossy(APP);
        assert!(!js.contains("crypto.subtle"));
        assert!(!js.contains("crypto.randomUUID"));
        assert!(js.contains("getRandomValues"));
        assert!(!js.contains("innerHTML"));
    }
}
