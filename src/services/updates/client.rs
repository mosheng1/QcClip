use std::{
    io::Read,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::config::{UpdateConfig, UpdateProxyMode};

pub(super) const RELEASES_API: &str = "https://api.github.com/repos/mosheng1/QcClip/releases";
pub(super) const GITEE_RELEASES_API: &str =
    "https://gitee.com/api/v5/repos/mk-mo-sheng/QcClip/releases";
const RELEASES_PAGE: &str = "https://github.com/mosheng1/QcClip/releases";
const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub(super) source: ReleaseSource,
    #[serde(default)]
    pub(super) assets: Vec<ReleaseAsset>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub(super) enum ReleaseSource {
    #[default]
    GitHub,
    Gitee,
}

impl ReleaseSource {
    pub(super) fn label(&self) -> &'static str {
        match self {
            Self::GitHub => "GitHub",
            Self::Gitee => "Gitee",
        }
    }

    pub(super) fn other(&self) -> Self {
        match self {
            Self::GitHub => Self::Gitee,
            Self::Gitee => Self::GitHub,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct ReleaseAsset {
    pub name: String,
    pub browser_download_url: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub digest: Option<String>,
}

impl Release {
    pub(crate) fn version_label(&self) -> String {
        format!("v{}", self.tag_name.trim().trim_start_matches(['v', 'V']))
    }

    pub(crate) fn url(&self) -> String {
        // 发布页由仓库地址与标签构造，不信任响应中的任意跳转地址。
        let tag = encode_path_segment(&self.tag_name);
        let releases_page = match &self.source {
            ReleaseSource::GitHub => RELEASES_PAGE,
            ReleaseSource::Gitee => "https://gitee.com/mk-mo-sheng/QcClip/releases",
        };
        format!("{releases_page}/tag/{tag}")
    }
}

fn encode_path_segment(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

pub(super) fn newer_than_current(release: &Release) -> bool {
    match (
        parse_version(&release.tag_name),
        parse_version(env!("CARGO_PKG_VERSION")),
    ) {
        (Some(latest), Some(current)) => latest.cmp_precedence(&current).is_gt(),
        _ => false,
    }
}

fn parse_version(tag: &str) -> Option<Version> {
    let tag = tag.trim().trim_start_matches(['v', 'V']);
    if let Ok(version) = Version::parse(tag) {
        // 将 beta10 等常见紧凑后缀转换为 beta.10，使数字按数值比较。
        if version.pre.is_empty() {
            return Some(version);
        }
    }
    let (without_build, build) = tag
        .split_once('+')
        .map_or((tag, None), |(a, b)| (a, Some(b)));
    let suffix_at = without_build
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit() && *c != '.')?
        .0;
    let core = without_build[..suffix_at].trim_end_matches('.');
    let suffix = without_build[suffix_at..].trim_start_matches(['-', '_', '.']);
    let mut normalized = String::new();
    let mut previous = None;
    for c in suffix.chars() {
        if let Some(p) = previous {
            if c.is_ascii_alphanumeric()
                && p != '.'
                && c.is_ascii_digit() != char::is_ascii_digit(&p)
            {
                normalized.push('.');
            }
        }
        let c = if c == '_' { '.' } else { c };
        normalized.push(c.to_ascii_lowercase());
        previous = Some(c);
    }
    let normalized = normalized
        .split('.')
        .map(|part| {
            if !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()) {
                let trimmed = part.trim_start_matches('0');
                if trimmed.is_empty() { "0" } else { trimmed }
            } else {
                part
            }
        })
        .collect::<Vec<_>>()
        .join(".");
    let mut version = format!("{core}-{normalized}");
    if let Some(build) = build {
        version.push('+');
        version.push_str(build);
    }
    Version::parse(&version).ok()
}

pub(crate) fn valid_proxy_url(url: &str) -> bool {
    ureq::Proxy::new(url).is_ok()
        && ureq::get(url).request_url().ok().is_some_and(|url| {
            url.scheme() == "http"
                && !url.host().is_empty()
                && url.as_url().path() == "/"
                && url.as_url().query().is_none()
                && url.as_url().fragment().is_none()
        })
}

pub(super) fn api_url() -> String {
    #[cfg(debug_assertions)]
    if let Ok(value) = std::env::var("QCCLIP_UPDATE_TEST_URL") {
        if valid_test_api_url(&value) {
            return value;
        }
        tracing::warn!("忽略无效的本地更新测试地址，仅允许本机 HTTP 服务");
    }
    RELEASES_API.to_owned()
}

#[cfg(debug_assertions)]
fn valid_test_api_url(value: &str) -> bool {
    ureq::get(value).request_url().ok().is_some_and(|url| {
        url.scheme() == "http"
            && matches!(url.host(), "127.0.0.1" | "localhost" | "[::1]")
            && url.as_url().username().is_empty()
            && url.as_url().password().is_none()
    })
}

fn fallback_api_url() -> String {
    #[cfg(debug_assertions)]
    if api_url() != RELEASES_API
        && let Ok(value) = std::env::var("QCCLIP_UPDATE_TEST_GITEE_URL")
        && valid_test_api_url(&value)
    {
        return value;
    }
    GITEE_RELEASES_API.to_owned()
}

pub(super) fn fetch_latest(config: &UpdateConfig) -> Result<Option<Release>, String> {
    let github_api = api_url();
    if github_api != RELEASES_API {
        let fallback = fallback_api_url();
        return if fallback != GITEE_RELEASES_API {
            fetch_with_fallback(config, &github_api, &fallback)
        } else {
            fetch_releases(config, &github_api)
        };
    }
    fetch_with_fallback(config, RELEASES_API, GITEE_RELEASES_API)
}

fn fetch_with_fallback(
    config: &UpdateConfig,
    primary_api: &str,
    fallback_api: &str,
) -> Result<Option<Release>, String> {
    match fetch_releases(config, primary_api) {
        Ok(latest) => Ok(latest),
        Err(primary_error) => {
            tracing::warn!("GitHub 更新检查失败，切换到 Gitee：{primary_error}");
            fetch_releases(config, fallback_api).map_err(|fallback_error| {
                format!("GitHub 和 Gitee 更新检查均失败：{primary_error}；{fallback_error}")
            })
        }
    }
}

pub(super) fn http_agent(config: &UpdateConfig, local_test: bool) -> Result<ureq::Agent, String> {
    let mut builder = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(8))
        .timeout_read(Duration::from_secs(15))
        .timeout_write(Duration::from_secs(15));
    if local_test {
        builder = builder.try_proxy_from_env(false);
    } else {
        builder = match config.proxy_mode {
            UpdateProxyMode::System => {
                let builder = builder.try_proxy_from_env(true);
                if let Some(proxy) = system_proxy() {
                    builder.proxy(proxy)
                } else {
                    builder
                }
            }
            UpdateProxyMode::Direct => builder.try_proxy_from_env(false),
            UpdateProxyMode::Custom => {
                if !valid_proxy_url(&config.proxy_url) {
                    return Err("代理地址无效，请检查代理设置".to_owned());
                }
                builder.try_proxy_from_env(false).proxy(
                    ureq::Proxy::new(&config.proxy_url)
                        .map_err(|_| "代理地址无效，请检查代理设置".to_owned())?,
                )
            }
        };
    }
    Ok(builder.build())
}

fn fetch_releases(config: &UpdateConfig, api: &str) -> Result<Option<Release>, String> {
    let agent = http_agent(config, api != RELEASES_API && api != GITEE_RELEASES_API)?;
    let mut latest: Option<(Version, Release)> = None;
    for page in 1..=20 {
        let response = match agent
            .get(api)
            .timeout(Duration::from_secs(12))
            .query("per_page", "100")
            .query("page", &page.to_string())
            .set("User-Agent", concat!("QcClip/", env!("CARGO_PKG_VERSION")))
            .set("Accept", "application/vnd.github+json")
            .call()
        {
            Ok(response) => response,
            Err(ureq::Error::Status(404, _)) if page == 1 && api != RELEASES_API => {
                return Ok(None);
            }
            Err(ureq::Error::Status(status @ (403 | 429), response)) => {
                return Err(access_error(service_name(api), status, &response));
            }
            Err(ureq::Error::Status(status, _)) => {
                return Err(format!("更新服务返回错误（HTTP {status}）"));
            }
            Err(_) => return Err("无法连接更新服务，请检查网络或代理设置".to_owned()),
        };
        let has_next = response
            .header("Link")
            .is_some_and(|link| link.contains("rel=\"next\""));
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(MAX_RESPONSE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "读取更新服务响应失败".to_owned())?;
        if bytes.len() as u64 > MAX_RESPONSE_BYTES {
            return Err("更新服务响应过大".to_owned());
        }
        let releases: Vec<Release> =
            serde_json::from_slice(&bytes).map_err(|_| "更新服务返回的数据格式错误".to_owned())?;
        for mut release in releases.into_iter().filter(|r| !r.draft) {
            release.source = source_for_api(api);
            let Some(version) = parse_version(&release.tag_name) else {
                continue;
            };
            if latest
                .as_ref()
                .is_none_or(|(previous, _)| version.cmp_precedence(previous).is_gt())
            {
                latest = Some((version, release));
            }
        }
        if !has_next {
            return Ok(latest.map(|(_, release)| release));
        }
    }
    Err("发布版本列表过长，未能完成更新检查".to_owned())
}

fn source_for_api(api: &str) -> ReleaseSource {
    if api == GITEE_RELEASES_API || api == fallback_api_url() {
        ReleaseSource::Gitee
    } else {
        ReleaseSource::GitHub
    }
}

fn service_name(api: &str) -> &'static str {
    if api == GITEE_RELEASES_API {
        "Gitee"
    } else {
        "GitHub"
    }
}

fn access_error(service: &str, status: u16, response: &ureq::Response) -> String {
    if response.header("X-RateLimit-Remaining") == Some("0") {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let minutes = response
            .header("X-RateLimit-Reset")
            .and_then(|value| value.parse::<u64>().ok())
            .map(|reset| reset.saturating_sub(now).div_ceil(60))
            .filter(|minutes| *minutes > 0);
        let recovery = minutes.map_or_else(
            || "请稍后重试".to_owned(),
            |minutes| format!("约 {minutes} 分钟后恢复"),
        );
        return format!(
            "当前网络出口的 {service} API 额度已用尽，{recovery}；可更换代理节点或使用直连"
        );
    }
    if status == 429 {
        format!("{service} 请求受限，请稍后重试或调整代理设置")
    } else {
        format!("{service} 拒绝访问更新接口（HTTP 403），请检查网络或代理设置")
    }
}

fn system_proxy() -> Option<ureq::Proxy> {
    if [
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "HTTP_PROXY",
        "http_proxy",
    ]
    .into_iter()
    .any(|name| std::env::var_os(name).is_some())
    {
        return None;
    }
    #[cfg(target_os = "windows")]
    {
        use winreg::{RegKey, enums::HKEY_CURRENT_USER};
        let key = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings")
            .ok()?;
        if key.get_value::<u32, _>("ProxyEnable").ok()? == 0 {
            return None;
        }
        let server: String = key.get_value("ProxyServer").ok()?;
        let address = if server.contains('=') {
            server
                .split(';')
                .filter_map(|entry| entry.trim().split_once('='))
                .find(|(scheme, _)| *scheme == "https")?
                .1
                .trim()
        } else {
            server.trim()
        };
        let address = if address.contains("://") {
            address.to_owned()
        } else {
            format!("http://{address}")
        };
        return ureq::Proxy::new(address).ok();
    }
    #[cfg(not(target_os = "windows"))]
    None
}

pub(super) fn fetch_mirror(
    tag: &str,
    config: &UpdateConfig,
    source: ReleaseSource,
) -> Result<Release, String> {
    let api = match source {
        ReleaseSource::GitHub => api_url(),
        ReleaseSource::Gitee => fallback_api_url(),
    };
    #[cfg(debug_assertions)]
    if api_url() != RELEASES_API && api == GITEE_RELEASES_API {
        return Err("未配置本地 Gitee 镜像测试地址".into());
    }
    let mut release = fetch_release_tag(config, &api, tag)?;
    release.source = source;
    Ok(release)
}

fn fetch_release_tag(config: &UpdateConfig, api: &str, tag: &str) -> Result<Release, String> {
    let endpoint = format!("{api}/tags/{}", encode_path_segment(tag));
    let response = http_agent(config, api != GITEE_RELEASES_API && api != RELEASES_API)?
        .get(&endpoint)
        .timeout(Duration::from_secs(12))
        .set("User-Agent", concat!("QcClip/", env!("CARGO_PKG_VERSION")))
        .call()
        .map_err(|e| format!("读取同版本镜像失败，可能尚未同步：{e}"))?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("读取镜像发布信息失败：{e}"))?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        return Err("镜像发布信息过大".into());
    }
    let mut release: Release =
        serde_json::from_slice(&bytes).map_err(|_| "镜像发布信息格式错误")?;
    if release.draft || release.tag_name != tag {
        return Err("镜像版本不一致或尚未发布".into());
    }
    release.source = source_for_api(api);
    Ok(release)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn compares_release_and_suffix_versions() {
        for (older, newer) in [
            ("v0.1.0", "v0.1.2-beta.1"),
            ("0.1.2-alpha1", "0.1.2-beta1"),
            ("0.1.2-beta2", "0.1.2-beta10"),
            ("0.1.2-rc2", "0.1.2"),
            ("0.1.2-nightly.2", "0.1.2-nightly.10"),
            ("0.1.2_preview02", "0.1.2_preview10"),
        ] {
            assert!(
                parse_version(newer)
                    .unwrap()
                    .cmp_precedence(&parse_version(older).unwrap())
                    .is_gt()
            );
        }
        assert!(
            parse_version("0.1.2+build.2")
                .unwrap()
                .cmp_precedence(&parse_version("0.1.2+build.1").unwrap())
                .is_eq()
        );
        assert!(parse_version("not-a-version").is_none());
    }

    #[test]
    fn validates_proxy_and_release_links() {
        assert!(valid_proxy_url("http://127.0.0.1:7890"));
        for invalid in [
            "",
            "file:///tmp/proxy",
            "socks5://localhost:1080",
            "http://",
            "https://localhost:7890",
            "http://localhost/proxy",
        ] {
            assert!(!valid_proxy_url(invalid));
        }
        let release = Release {
            tag_name: "v0.2.0+test/1".into(),
            draft: false,
            prerelease: true,
            source: ReleaseSource::default(),
            assets: Vec::new(),
        };
        assert_eq!(
            release.url(),
            "https://github.com/mosheng1/QcClip/releases/tag/v0.2.0%2Btest%2F1"
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    fn debug_test_endpoint_only_accepts_loopback_http() {
        for url in [
            "http://127.0.0.1:18765/releases",
            "http://localhost:18765/releases",
            "http://[::1]:18765/releases",
        ] {
            assert!(valid_test_api_url(url));
        }
        for url in [
            "https://127.0.0.1/releases",
            "http://example.com/releases",
            "http://localhost.example.com/releases",
            "http://127.0.0.1@example.com/releases",
            "http://user@localhost/releases",
            "invalid",
        ] {
            assert!(!valid_test_api_url(url));
        }
    }

    fn mock_response(status: u16, body: &str) -> String {
        format!(
            "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn fetch_mock(response: String) -> Result<Option<Release>, String> {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/releases", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 2048];
            assert!(stream.read(&mut request).unwrap() > 0);
            stream.write_all(response.as_bytes()).unwrap();
        });
        let result = fetch_releases(&UpdateConfig::default(), &url);
        server.join().unwrap();
        result
    }

    fn fetch_with_fallback_mock(
        primary_response: String,
        fallback_response: String,
    ) -> Result<Option<Release>, String> {
        use std::net::TcpListener;
        let primary_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let fallback_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let primary_url = format!("http://{}/releases", primary_listener.local_addr().unwrap());
        let fallback_url = format!(
            "http://{}/releases",
            fallback_listener.local_addr().unwrap()
        );
        let primary_server = std::thread::spawn(move || {
            let (mut stream, _) = primary_listener.accept().unwrap();
            let mut request = [0; 2048];
            assert!(stream.read(&mut request).unwrap() > 0);
            stream.write_all(primary_response.as_bytes()).unwrap();
        });
        let fallback_server = std::thread::spawn(move || {
            let (mut stream, _) = fallback_listener.accept().unwrap();
            let mut request = [0; 2048];
            assert!(stream.read(&mut request).unwrap() > 0);
            stream.write_all(fallback_response.as_bytes()).unwrap();
        });
        let result = fetch_with_fallback(&UpdateConfig::default(), &primary_url, &fallback_url);
        primary_server.join().unwrap();
        fallback_server.join().unwrap();
        result
    }

    #[test]
    fn handles_empty_and_unpublished_repositories() {
        assert!(fetch_mock(mock_response(200, "[]")).unwrap().is_none());
        assert!(fetch_mock(mock_response(404, "{}")).unwrap().is_none());
    }

    #[test]
    fn chooses_newest_version_including_prereleases_and_excludes_drafts() {
        let release = fetch_mock(mock_response(
            200,
            r#"[
            {"tag_name":"v1.1.0-beta2", "prerelease":true},
            {"tag_name":"v99.0.0", "draft":true},
            {"tag_name":"invalid-tag"},
            {"tag_name":"v1.0.0"},
            {"tag_name":"v1.1.0-beta10", "prerelease":true}
        ]"#,
        ))
        .unwrap()
        .unwrap();
        assert_eq!(release.tag_name, "v1.1.0-beta10");
        assert!(newer_than_current(&release));
    }

    #[test]
    fn rejects_failed_and_invalid_responses() {
        assert!(
            fetch_mock(mock_response(429, "{}"))
                .unwrap_err()
                .contains("请求受限")
        );
        assert!(
            fetch_mock(mock_response(500, "{}"))
                .unwrap_err()
                .contains("HTTP 500")
        );
        assert!(
            fetch_mock(mock_response(200, "not-json"))
                .unwrap_err()
                .contains("格式错误")
        );
    }

    #[test]
    fn falls_back_to_gitee_after_github_failure() {
        let release = fetch_with_fallback_mock(
            mock_response(500, "{}"),
            mock_response(200, r#"[{"tag_name":"v1.2.0"}]"#),
        )
        .unwrap()
        .unwrap();
        assert_eq!(release.tag_name, "v1.2.0");
    }

    #[test]
    fn uses_the_source_release_page_for_gitee_releases() {
        let release = Release {
            tag_name: "v1.2.0".into(),
            draft: false,
            prerelease: false,
            source: ReleaseSource::Gitee,
            assets: Vec::new(),
        };
        assert_eq!(
            release.url(),
            "https://gitee.com/mk-mo-sheng/QcClip/releases/tag/v1.2.0"
        );
    }

    #[test]
    fn mirror_lookup_requires_the_exact_published_tag() {
        use std::net::TcpListener;
        for body in [
            r#"{"tag_name":"v1.2.0"}"#,
            r#"{"tag_name":"v1.1.0"}"#,
            r#"{"tag_name":"v1.2.0","draft":true}"#,
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let api = format!("http://{}/releases", listener.local_addr().unwrap());
            let response = mock_response(200, body);
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0; 2048];
                let count = stream.read(&mut request).unwrap();
                assert!(
                    String::from_utf8_lossy(&request[..count]).contains("/releases/tags/v1.2.0")
                );
                stream.write_all(response.as_bytes()).unwrap();
            });
            let result = fetch_release_tag(&UpdateConfig::default(), &api, "v1.2.0");
            server.join().unwrap();
            assert_eq!(
                result.is_ok(),
                !body.contains("v1.1.0") && !body.contains("draft")
            );
        }
    }

    #[test]
    fn reports_both_sources_when_fallback_fails() {
        let error = fetch_with_fallback_mock(mock_response(500, "{}"), mock_response(503, "{}"))
            .unwrap_err();
        assert!(error.contains("GitHub 和 Gitee 更新检查均失败"));
        assert!(error.contains("HTTP 500"));
        assert!(error.contains("HTTP 503"));
    }

    #[test]
    fn reports_exhausted_github_quota_and_recovery_time() {
        let reset = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600;
        let response = mock_response(403, "{}").replacen(
            "Content-Type:",
            &format!("X-RateLimit-Remaining: 0\r\nX-RateLimit-Reset: {reset}\r\nContent-Type:"),
            1,
        );
        let error = fetch_mock(response).unwrap_err();
        assert!(error.contains("额度已用尽"));
        assert!(error.contains("分钟后恢复"));
    }

    #[test]
    fn distinguishes_access_denied_from_exhausted_quota() {
        let error = fetch_mock(mock_response(403, "{}")).unwrap_err();
        assert!(error.contains("HTTP 403"));
        assert!(!error.contains("额度已用尽"));

        let response = mock_response(403, "{}").replacen(
            "Content-Type:",
            "X-RateLimit-Remaining: 0\r\nX-RateLimit-Reset: 0\r\nContent-Type:",
            1,
        );
        let error = fetch_mock(response).unwrap_err();
        assert!(error.contains("额度已用尽"));
        assert!(!error.contains("分钟后恢复"));
    }

    #[test]
    fn follows_release_pagination() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/releases", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            for page in 1..=2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0; 2048];
                let size = stream.read(&mut request).unwrap();
                assert!(
                    String::from_utf8_lossy(&request[..size]).contains(&format!("page={page}"))
                );
                let body = if page == 1 {
                    r#"[{"tag_name":"v0.1.0"}]"#
                } else {
                    r#"[{"tag_name":"v0.3.0-rc2","prerelease":true}]"#
                };
                let mut response = mock_response(200, body);
                if page == 1 {
                    response = response.replacen(
                        "Content-Type:",
                        "Link: <http://localhost/releases?page=2>; rel=\"next\"\r\nContent-Type:",
                        1,
                    );
                }
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        let result = fetch_releases(&UpdateConfig::default(), &url);
        server.join().unwrap();
        assert_eq!(result.unwrap().unwrap().tag_name, "v0.3.0-rc2");
    }
}
