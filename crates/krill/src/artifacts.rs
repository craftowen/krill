//! `krill artifacts`: HTML·Markdown 파일을 share.krill.page에 공유·갱신·회수·조회·삭제한다.
//!
//! Orca `orca artifacts` CLI를 본뜬다. HTTP 크레이트를 늘리지 않으려고 `curl`을 자식 프로세스로 부른다.
//! 토큰이 `ps`에 보이지 않게 헤더는 0600 임시 파일(`-H @file`)로, 본문은 stdin으로 넘긴다.

use krill_proto::os::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs::{self, DirBuilder, OpenOptions};
#[cfg(not(windows))]
use std::io::Read as _;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use krill_proto::daemon;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const USAGE: &str =
    "usage: krill artifacts share <file> [--title <title>] [--api-url <url>] [--json]
       krill artifacts update <file> [--title <title>] [--api-url <url>] [--json]
       krill artifacts unshare <file> [--api-url <url>] [--json]
       krill artifacts list [--cursor <cursor>] [--api-url <url>] [--json]
       krill artifacts delete|rm <slug> [--api-url <url>] [--json]";
const DEFAULT_API_URL: &str = "https://share.krill.page";
const MAX_BYTES: u64 = 2 * 1024 * 1024;
const TOKEN_FILE: &str = "artifact-token";
const SHARES_FILE: &str = "artifact-shares.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Record {
    slug: String,
    edit_token: String,
    share_url: String,
    api_origin: String,
    #[serde(default)]
    expires_at: String,
}

type Shares = BTreeMap<String, Record>;

struct Ctx {
    state_dir: PathBuf,
    curl: OsString,
    api_url: String,
}

#[derive(Default)]
struct Opts {
    positional: Vec<String>,
    title: Option<String>,
    api_url: Option<String>,
    cursor: Option<String>,
    json: bool,
}

#[derive(Debug)]
enum Error {
    Usage(String),
    Fail(String),
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Fail(e.to_string())
    }
}

pub fn run(raw: &[OsString]) -> ExitCode {
    let Some(state_dir) = daemon::state_dir() else {
        eprintln!("krill artifacts: cannot resolve state dir (set HOME or KRILL_STATE_DIR)");
        return ExitCode::FAILURE;
    };
    let ctx = Ctx {
        state_dir,
        curl: std::env::var_os("KRILL_CURL_BIN").unwrap_or_else(default_curl),
        api_url: std::env::var("KRILL_ARTIFACTS_API_URL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_API_URL.to_owned()),
    };
    match execute(&ctx, raw) {
        Ok(out) => {
            println!("{out}");
            ExitCode::SUCCESS
        }
        Err(Error::Usage(msg)) => {
            eprintln!("{msg}");
            ExitCode::from(2)
        }
        Err(Error::Fail(msg)) => {
            eprintln!("krill artifacts: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn default_curl() -> OsString {
    if Path::new("/usr/bin/curl").exists() {
        "/usr/bin/curl".into()
    } else {
        "curl".into()
    }
}

fn execute(ctx: &Ctx, raw: &[OsString]) -> Result<String, Error> {
    let args = raw
        .iter()
        .map(|s| s.to_str().map(str::to_owned))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| Error::Usage("arguments must be UTF-8".into()))?;
    // 서브커맨드 자리만 도움말로 본다. `--title help` 같은 옵션값은 제목이다.
    let help_here = matches!(
        args.first().map(String::as_str),
        Some("-h" | "--help" | "help")
    ) || matches!(args.get(1), Some(s) if matches!(s.as_str(), "-h" | "--help"));
    if help_here {
        return Ok(USAGE.into());
    }
    let Some((cmd, rest)) = args.split_first() else {
        return Err(Error::Usage(USAGE.into()));
    };
    let opts = parse(rest)?;
    let api = opts
        .api_url
        .as_deref()
        .unwrap_or(&ctx.api_url)
        .trim_end_matches('/')
        .to_owned();
    if !(api.starts_with("https://") || api.starts_with("http://")) {
        return Err(Error::Usage(format!("invalid --api-url: {api}")));
    }
    let one = |what: &str| match opts.positional.as_slice() {
        [v] => Ok(v.clone()),
        _ => Err(Error::Usage(format!("missing or extra <{what}>\n{USAGE}"))),
    };
    match cmd.as_str() {
        "share" => share(ctx, &api, &one("file")?, &opts, false),
        "update" => share(ctx, &api, &one("file")?, &opts, true),
        "unshare" => unshare(ctx, &api, &one("file")?, &opts),
        "list" | "ls" if opts.positional.is_empty() => list(ctx, &api, &opts),
        "delete" | "rm" => delete(ctx, &api, &one("slug")?, &opts),
        _ => Err(Error::Usage(USAGE.into())),
    }
}

fn parse(rest: &[String]) -> Result<Opts, Error> {
    let mut opts = Opts::default();
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| Error::Usage(format!("{name} needs a value\n{USAGE}")))
        };
        match arg.as_str() {
            "--json" => opts.json = true,
            "--title" => opts.title = Some(value("--title")?),
            "--api-url" => opts.api_url = Some(value("--api-url")?),
            "--cursor" => opts.cursor = Some(value("--cursor")?),
            s if s.starts_with("--") => {
                return Err(Error::Usage(format!("unknown flag {s}\n{USAGE}")));
            }
            s => opts.positional.push(s.to_owned()),
        }
    }
    Ok(opts)
}

fn share(
    ctx: &Ctx,
    api: &str,
    file: &str,
    opts: &Opts,
    update_only: bool,
) -> Result<String, Error> {
    let (key, body) = read_artifact(file, opts.title.as_deref())?;
    let token = owner_token(&ctx.state_dir)?;
    let mut shares = load_shares(&ctx.state_dir)?;
    let existing = shares.get(&key).filter(|r| r.api_origin == api).cloned();
    if update_only && existing.is_none() {
        return Err(Error::Fail(format!(
            "{file} has not been shared to {api}; run `krill artifacts share` first"
        )));
    }
    if let Some(rec) = existing {
        let url = format!("{api}/v1/artifacts/{}", rec.slug);
        let (status, resp) = request(ctx, "PUT", &url, &token, Some(&rec.edit_token), Some(&body))?;
        if status != 404 || update_only {
            let v = expect_ok(status, &resp)?;
            let rec = Record {
                share_url: str_field(&v, "shareUrl").unwrap_or(rec.share_url),
                expires_at: v["artifact"]["expiresAt"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                ..rec
            };
            shares.insert(key, rec.clone());
            save_shares(&ctx.state_dir, &shares)?;
            return Ok(share_output(&rec, "updated", opts.json));
        }
        // 서버에서 만료·삭제됐다. 새로 만든다.
    }
    let (status, resp) = request(
        ctx,
        "POST",
        &format!("{api}/v1/artifacts"),
        &token,
        None,
        Some(&body),
    )?;
    let v = expect_ok(status, &resp)?;
    let (Some(slug), Some(edit_token), Some(share_url)) = (
        v["artifact"]["slug"].as_str(),
        str_field(&v, "editToken"),
        str_field(&v, "shareUrl"),
    ) else {
        return Err(Error::Fail(format!("unexpected server response: {resp}")));
    };
    let rec = Record {
        slug: slug.to_owned(),
        edit_token,
        share_url,
        api_origin: api.to_owned(),
        expires_at: v["artifact"]["expiresAt"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
    };
    shares.insert(key, rec.clone());
    save_shares(&ctx.state_dir, &shares)?;
    Ok(share_output(&rec, "created", opts.json))
}

fn unshare(ctx: &Ctx, api: &str, file: &str, opts: &Opts) -> Result<String, Error> {
    let key = canonical(file)?;
    let mut shares = load_shares(&ctx.state_dir)?;
    let Some(rec) = shares.get(&key).filter(|r| r.api_origin == api).cloned() else {
        return Err(Error::Fail(format!("{file} has not been shared to {api}")));
    };
    let token = owner_token(&ctx.state_dir)?;
    let url = format!("{api}/v1/artifacts/{}", rec.slug);
    let (status, resp) = request(ctx, "DELETE", &url, &token, Some(&rec.edit_token), None)?;
    // 404면 이미 서버에 없다. 로컬 기록만 지운다.
    if status != 404 {
        expect_ok(status, &resp)?;
    }
    shares.remove(&key);
    save_shares(&ctx.state_dir, &shares)?;
    Ok(deleted_output(&rec.slug, opts.json))
}

fn list(ctx: &Ctx, api: &str, opts: &Opts) -> Result<String, Error> {
    let token = owner_token(&ctx.state_dir)?;
    let mut url = format!("{api}/v1/artifacts");
    if let Some(cursor) = &opts.cursor {
        url.push_str("?cursor=");
        url.push_str(&percent_encode(cursor));
    }
    let (status, resp) = request(ctx, "GET", &url, &token, None, None)?;
    let v = expect_ok(status, &resp)?;
    if opts.json {
        return Ok(v.to_string());
    }
    let items = v["artifacts"].as_array().cloned().unwrap_or_default();
    if items.is_empty() {
        return Ok("no artifacts".into());
    }
    let mut out: Vec<String> = items
        .iter()
        .map(|it| {
            let a = &it["artifact"];
            let name = a["title"]
                .as_str()
                .or_else(|| a["originalFileName"].as_str())
                .unwrap_or("Untitled");
            format!(
                "{}  {}  {}  expires {}",
                a["slug"].as_str().unwrap_or("?"),
                name,
                it["shareUrl"].as_str().unwrap_or(""),
                a["expiresAt"].as_str().unwrap_or("?"),
            )
        })
        .collect();
    if let Some(next) = v["nextCursor"].as_str() {
        out.push(format!("next: krill artifacts list --cursor {next}"));
    }
    Ok(out.join("\n"))
}

fn delete(ctx: &Ctx, api: &str, slug: &str, opts: &Opts) -> Result<String, Error> {
    if slug.is_empty()
        || !slug
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(Error::Usage(format!("invalid slug: {slug}")));
    }
    let token = owner_token(&ctx.state_dir)?;
    let mut shares = load_shares(&ctx.state_dir)?;
    let edit = shares
        .values()
        .find(|r| r.slug == slug && r.api_origin == api)
        .map(|r| r.edit_token.clone());
    let url = format!("{api}/v1/artifacts/{}", percent_encode(slug));
    let (status, resp) = request(ctx, "DELETE", &url, &token, edit.as_deref(), None)?;
    expect_ok(status, &resp)?;
    let before = shares.len();
    shares.retain(|_, r| !(r.slug == slug && r.api_origin == api));
    if shares.len() != before {
        save_shares(&ctx.state_dir, &shares)?;
    }
    Ok(deleted_output(slug, opts.json))
}

fn share_output(rec: &Record, change: &str, as_json: bool) -> String {
    if as_json {
        json!({
            "change": change,
            "slug": rec.slug,
            "shareUrl": rec.share_url,
            "expiresAt": rec.expires_at,
        })
        .to_string()
    } else {
        rec.share_url.clone()
    }
}

fn deleted_output(slug: &str, as_json: bool) -> String {
    if as_json {
        json!({ "deleted": true, "slug": slug }).to_string()
    } else {
        format!("deleted {slug}")
    }
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v[key].as_str().map(str::to_owned)
}

fn canonical(file: &str) -> Result<String, Error> {
    let path = fs::canonicalize(file).map_err(|e| Error::Fail(format!("{file}: {e}")))?;
    path.into_os_string()
        .into_string()
        .map_err(|_| Error::Fail(format!("{file}: path must be UTF-8")))
}

/// 파일을 검증하고 (정규 경로, 요청 본문 JSON)을 돌려준다.
fn read_artifact(file: &str, title: Option<&str>) -> Result<(String, String), Error> {
    let key = canonical(file)?;
    let path = Path::new(&key);
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let content_type = match ext.as_str() {
        "html" | "htm" => "text/html",
        "md" | "markdown" => "text/markdown",
        _ => {
            return Err(Error::Usage(format!(
                "{file}: only .html, .htm, .md, .markdown files can be shared"
            )));
        }
    };
    let meta = fs::metadata(path)?;
    if !meta.is_file() {
        return Err(Error::Fail(format!("{file}: not a regular file")));
    }
    if meta.len() > MAX_BYTES {
        return Err(Error::Fail(format!("{file}: larger than 2 MiB")));
    }
    let content = fs::read_to_string(path).map_err(|e| Error::Fail(format!("{file}: {e}")))?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("artifact");
    let mut body =
        json!({ "content": content, "contentType": content_type, "fileName": file_name });
    if let Some(t) = title {
        body["title"] = json!(t);
    }
    Ok((key, body.to_string()))
}

/// 상태 디렉터리의 영구 소유자 토큰. 없으면 OS CSPRNG 32바이트로 만든다(0600).
fn owner_token(dir: &Path) -> Result<String, Error> {
    let path = dir.join(TOKEN_FILE);
    match fs::read_to_string(&path) {
        Ok(t) if !t.trim().is_empty() => return Ok(t.trim().to_owned()),
        Ok(_) => fs::remove_file(&path)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let mut bytes = [0u8; 32];
    #[cfg(not(windows))]
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    #[cfg(windows)]
    {
        use ring::rand::SecureRandom as _;
        ring::rand::SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| io::Error::other("OS random source unavailable"))?;
    }
    let token = bytes.iter().fold(String::new(), |mut t, b| {
        write!(t, "{b:02x}").ok();
        t
    });
    DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(mut f) => {
            f.write_all(token.as_bytes())?;
            Ok(token)
        }
        // 동시에 다른 호출이 먼저 만들었다.
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            Ok(fs::read_to_string(&path)?.trim().to_owned())
        }
        Err(e) => Err(e.into()),
    }
}

fn load_shares(dir: &Path) -> Result<Shares, Error> {
    match fs::read_to_string(dir.join(SHARES_FILE)) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|e| Error::Fail(format!("{SHARES_FILE} is corrupt: {e}"))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Shares::new()),
        Err(e) => Err(e.into()),
    }
}

// ponytail: 잠금 없음. 동시 share 두 개가 겹치면 나중 쓰기가 이긴다(편집 토큰 하나 유실 가능). 문제가 되면 schedule.rs식 lock 파일.
fn save_shares(dir: &Path, shares: &Shares) -> Result<(), Error> {
    let text = serde_json::to_string_pretty(shares).map_err(|e| Error::Fail(e.to_string()))?;
    DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let tmp = dir.join(format!("{SHARES_FILE}.{}.tmp", std::process::id()));
    let result = (|| {
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_data()?;
        fs::rename(&tmp, dir.join(SHARES_FILE))
    })();
    if result.is_err() {
        fs::remove_file(&tmp).ok();
    }
    Ok(result?)
}

/// curl로 요청하고 (HTTP 상태, 응답 본문)을 돌려준다.
fn request(
    ctx: &Ctx,
    method: &str,
    url: &str,
    token: &str,
    edit_token: Option<&str>,
    body: Option<&str>,
) -> Result<(u16, String), Error> {
    let mut headers = format!("Authorization: Bearer {token}\n");
    if let Some(edit) = edit_token {
        writeln!(headers, "x-krill-edit-token: {edit}").ok();
    }
    if body.is_some() {
        headers.push_str("Content-Type: application/json\n");
    }
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&ctx.state_dir)?;
    let header_file = ctx
        .state_dir
        .join(format!("artifact-curl-{}.headers", std::process::id()));
    let result = (|| {
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&header_file)?;
        f.write_all(headers.as_bytes())?;
        drop(f);
        let mut cmd = Command::new(&ctx.curl);
        cmd.args([
            "-sS",
            "--max-time",
            "120",
            "-X",
            method,
            "-w",
            "\\n%{http_code}",
            "-H",
        ])
        .arg(format!("@{}", header_file.display()));
        if body.is_some() {
            cmd.args(["--data-binary", "@-"]);
        }
        cmd.arg(url)
            .stdin(if body.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn()?;
        if let (Some(b), Some(mut stdin)) = (body, child.stdin.take()) {
            stdin.write_all(b.as_bytes())?;
        }
        child.wait_with_output()
    })();
    fs::remove_file(&header_file).ok();
    let out = result.map_err(|e| {
        Error::Fail(format!(
            "cannot run curl ({}): {e}",
            ctx.curl.to_string_lossy()
        ))
    })?;
    if !out.status.success() {
        return Err(Error::Fail(format!(
            "{method} {url} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let (resp, code) = text.rsplit_once('\n').unwrap_or(("", &text));
    let status = code
        .trim()
        .parse()
        .map_err(|_| Error::Fail(format!("{method} {url}: unexpected curl output")))?;
    Ok((status, resp.to_owned()))
}

/// 2xx면 JSON(빈 본문은 null)을, 아니면 서버 오류 코드를 담은 오류를 돌려준다.
fn expect_ok(status: u16, resp: &str) -> Result<Value, Error> {
    let v: Value = if resp.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(resp).unwrap_or_else(|_| Value::String(resp.to_owned()))
    };
    if (200..300).contains(&status) {
        return Ok(v);
    }
    let code = v["code"].as_str().unwrap_or(resp.trim());
    Err(Error::Fail(format!(
        "server returned HTTP {status}: {code}"
    )))
}

fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    /// 요청을 `log`에 남기고 메서드·URL별로 정해진 응답을 내는 가짜 curl.
    const MOCK: &str = r#"#!/bin/sh
dir=$(dirname "$0")
method=GET; url=; hdr=
while [ $# -gt 0 ]; do
  case "$1" in
    -X) method=$2; shift ;;
    -H) hdr=${2#@}; shift ;;
    -w|--max-time|--data-binary) shift ;;
    -*) ;;
    *) url=$1 ;;
  esac
  shift
done
{ echo "$method $url"; cat "$hdr"; } >> "$dir/log"
[ "$method" = POST ] || [ "$method" = PUT ] && cat > "$dir/body"
case "$method $url" in
  "POST http://mock/v1/artifacts")
    printf '{"artifact":{"slug":"s1","expiresAt":"2026-11-01"},"shareUrl":"http://mock/a/s1","editToken":"e1"}\n201' ;;
  "PUT http://mock/v1/artifacts/s1")
    printf '{"artifact":{"slug":"s1","expiresAt":"2026-11-02"},"shareUrl":"http://mock/a/s1"}\n200' ;;
  "PUT http://mock/v1/artifacts/gone")
    printf '{"code":"not_found"}\n404' ;;
  "DELETE http://mock/v1/artifacts/"*)
    printf '\n204' ;;
  "GET http://mock/v1/artifacts"*)
    printf '{"artifacts":[{"artifact":{"slug":"s1","title":"T","expiresAt":"x"},"shareUrl":"http://mock/a/s1"}],"nextCursor":"c2"}\n200' ;;
  *) printf '{"code":"not_found"}\n404' ;;
esac
"#;

    struct Env {
        dir: PathBuf,
        ctx: Ctx,
    }

    impl Env {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("krill-artifacts-{name}-{}", std::process::id()));
            fs::remove_dir_all(&dir).ok();
            fs::create_dir_all(&dir).unwrap();
            let curl = dir.join("curl");
            fs::write(&curl, MOCK).unwrap();
            fs::set_permissions(&curl, fs::Permissions::from_mode(0o755)).unwrap();
            let ctx = Ctx {
                state_dir: dir.join("state"),
                curl: curl.into_os_string(),
                api_url: "http://mock/".into(),
            };
            Self { dir, ctx }
        }

        fn run(&self, args: &[&str]) -> Result<String, Error> {
            let raw: Vec<OsString> = args.iter().map(OsString::from).collect();
            execute(&self.ctx, &raw)
        }

        fn file(&self, name: &str, text: &str) -> String {
            let p = self.dir.join(name);
            fs::write(&p, text).unwrap();
            p.to_str().unwrap().to_owned()
        }

        fn log(&self) -> String {
            fs::read_to_string(self.dir.join("log")).unwrap_or_default()
        }

        fn shares(&self) -> Shares {
            load_shares(&self.ctx.state_dir).unwrap()
        }
    }

    impl Drop for Env {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.dir).ok();
        }
    }

    fn mode(p: &Path) -> u32 {
        fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn share_creates_then_reuses_record_and_update_puts() {
        let env = Env::new("share");
        let f = env.file("r.md", "# hi");
        assert_eq!(
            env.run(&["share", &f, "--title", "Report"]).unwrap(),
            "http://mock/a/s1"
        );
        let body: Value =
            serde_json::from_str(&fs::read_to_string(env.dir.join("body")).unwrap()).unwrap();
        assert_eq!(body["contentType"], "text/markdown");
        assert_eq!(body["fileName"], "r.md");
        assert_eq!(body["title"], "Report");
        assert_eq!(body["content"], "# hi");
        let token = fs::read_to_string(env.ctx.state_dir.join(TOKEN_FILE)).unwrap();
        assert_eq!(token.len(), 64);
        assert_eq!(mode(&env.ctx.state_dir.join(TOKEN_FILE)), 0o600);
        assert_eq!(mode(&env.ctx.state_dir.join(SHARES_FILE)), 0o600);
        let key = canonical(&f).unwrap();
        let rec = env.shares()[&key].clone();
        assert_eq!(
            (
                rec.slug.as_str(),
                rec.edit_token.as_str(),
                rec.api_origin.as_str()
            ),
            ("s1", "e1", "http://mock")
        );
        assert!(
            env.log()
                .contains(&format!("Authorization: Bearer {token}"))
        );

        // 다시 share하면 기록된 slug에 PUT한다.
        let out = env.run(&["share", &f, "--json"]).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["change"], "updated");
        assert_eq!(v["expiresAt"], "2026-11-02");
        assert!(env.log().contains("PUT http://mock/v1/artifacts/s1\n"));
        assert!(env.log().contains("x-krill-edit-token: e1"));

        let out = env.run(&["update", &f, "--json"]).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&out).unwrap()["slug"], "s1");
        assert_eq!(env.log().matches("POST ").count(), 1);
        // 헤더 임시 파일은 남지 않는다.
        assert!(!fs::read_dir(&env.ctx.state_dir).unwrap().any(|e| {
            e.unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".headers")
        }));
    }

    #[test]
    fn share_recreates_when_server_lost_artifact() {
        let env = Env::new("recreate");
        let f = env.file("r.html", "<p>x</p>");
        let key = canonical(&f).unwrap();
        let stale = Record {
            slug: "gone".into(),
            edit_token: "old".into(),
            share_url: "http://mock/a/gone".into(),
            api_origin: "http://mock".into(),
            expires_at: String::new(),
        };
        save_shares(&env.ctx.state_dir, &Shares::from([(key.clone(), stale)])).unwrap();
        assert_eq!(env.run(&["share", &f]).unwrap(), "http://mock/a/s1");
        assert!(env.log().contains("PUT http://mock/v1/artifacts/gone"));
        assert!(env.log().contains("POST http://mock/v1/artifacts"));
        assert_eq!(env.shares()[&key].slug, "s1");
    }

    #[test]
    fn update_and_unshare_require_record_for_same_origin() {
        let env = Env::new("norecord");
        let f = env.file("r.md", "x");
        assert!(matches!(env.run(&["update", &f]), Err(Error::Fail(_))));
        env.run(&["share", &f]).unwrap();
        assert!(matches!(
            env.run(&["update", &f, "--api-url", "http://other"]),
            Err(Error::Fail(_))
        ));
        assert!(matches!(
            env.run(&["unshare", &f, "--api-url", "http://other"]),
            Err(Error::Fail(_))
        ));
    }

    #[test]
    fn unshare_deletes_with_edit_token_and_forgets_record() {
        let env = Env::new("unshare");
        let f = env.file("r.md", "x");
        env.run(&["share", &f]).unwrap();
        let out = env.run(&["unshare", &f, "--json"]).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&out).unwrap(),
            json!({"deleted": true, "slug": "s1"})
        );
        assert!(
            env.log()
                .contains("DELETE http://mock/v1/artifacts/s1\nAuthorization: Bearer ")
        );
        assert!(env.log().ends_with("x-krill-edit-token: e1\n"));
        assert!(env.shares().is_empty());
    }

    #[test]
    fn list_formats_and_passes_cursor() {
        let env = Env::new("list");
        let out = env.run(&["list", "--cursor", "a b="]).unwrap();
        assert!(
            env.log()
                .contains("GET http://mock/v1/artifacts?cursor=a%20b%3D")
        );
        assert_eq!(
            out,
            "s1  T  http://mock/a/s1  expires x\nnext: krill artifacts list --cursor c2"
        );
        let v: Value = serde_json::from_str(&env.run(&["list", "--json"]).unwrap()).unwrap();
        assert_eq!(v["nextCursor"], "c2");
    }

    #[test]
    fn delete_by_slug_removes_matching_record() {
        let env = Env::new("delete");
        let f = env.file("r.md", "x");
        env.run(&["share", &f]).unwrap();
        assert_eq!(env.run(&["rm", "s1"]).unwrap(), "deleted s1");
        let log = env.log();
        let last = log.rsplit("DELETE").next().unwrap();
        assert!(last.starts_with(" http://mock/v1/artifacts/s1\nAuthorization: Bearer "));
        assert!(last.ends_with("x-krill-edit-token: e1\n"));
        assert!(env.shares().is_empty());
        let out = env.run(&["delete", "other", "--json"]).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&out).unwrap()["slug"],
            "other"
        );
    }

    #[test]
    fn help_returns_usage_string() {
        let env = Env::new("help");
        assert!(
            env.run(&["--help"])
                .unwrap()
                .contains("krill artifacts share")
        );
        assert!(
            env.run(&["share", "--help"])
                .unwrap()
                .contains("krill artifacts share")
        );
        assert!(
            env.run(&["list", "-h"])
                .unwrap()
                .contains("krill artifacts share")
        );
    }

    #[test]
    fn rejects_bad_input() {
        let env = Env::new("bad");
        let txt = env.file("r.txt", "x");
        assert!(matches!(env.run(&["share", &txt]), Err(Error::Usage(_))));
        assert!(matches!(env.run(&[]), Err(Error::Usage(_))));
        assert!(matches!(env.run(&["share"]), Err(Error::Usage(_))));
        assert!(matches!(
            env.run(&["share", "--bogus"]),
            Err(Error::Usage(_))
        ));
        assert!(matches!(
            env.run(&["list", "--cursor"]),
            Err(Error::Usage(_))
        ));
        assert!(matches!(
            env.run(&["list", "--api-url", "ftp://x"]),
            Err(Error::Usage(_))
        ));
        assert!(matches!(env.run(&["delete", ".."]), Err(Error::Usage(_))));
        let big = env.dir.join("big.html");
        fs::File::create(&big)
            .unwrap()
            .set_len(MAX_BYTES + 1)
            .unwrap();
        assert!(matches!(
            env.run(&["share", big.to_str().unwrap()]),
            Err(Error::Fail(_))
        ));
        assert!(env.log().is_empty());
        assert!(
            env.run(&["--help"])
                .unwrap()
                .contains("krill artifacts share")
        );
    }
}

#[cfg(all(test, windows))]
mod windows_random_tests {
    #[test]
    fn artifact_owner_token_uses_os_random_and_is_reused() {
        let dir = std::env::temp_dir().join(format!("owe917-token-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let token = super::owner_token(&dir).unwrap();
        assert_eq!(token.len(), 64);
        assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(super::owner_token(&dir).unwrap(), token);
        std::fs::remove_dir_all(dir).ok();
    }
}
