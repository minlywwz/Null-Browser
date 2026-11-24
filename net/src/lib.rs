use std::{
    io::{self, Read},
    net::IpAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use encoding_rs::{Encoding, UTF_8};
use reqwest::{
    blocking::{Client, ClientBuilder, Response},
    dns::{Addrs, Name, Resolve, Resolving},
    header::{ACCEPT, ACCEPT_LANGUAGE, CONTENT_TYPE, LOCATION},
    redirect::Policy,
};
pub use url::Url;

pub const MAX_URL_LENGTH: usize = 8 * 1024;
pub const MAX_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_STYLESHEET_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_SCRIPT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_FONT_BYTES: usize = 4 * 1024 * 1024;
const MAX_REDIRECTS: usize = 6;

pub fn address(input: &str) -> Result<Url, String> {
    let input = input.trim();
    if input.is_empty()
        || input.len() > MAX_URL_LENGTH
        || input.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err("Enter a domain or a complete HTTP(S) address.".into());
    }
    let url = if input.contains("://") {
        Url::parse(input)
    } else {
        Url::parse(&format!("https://{input}"))
    }
    .map_err(|_| "Enter a valid domain or HTTP(S) address.")?;
    validate(&url)?;
    Ok(url)
}

pub fn validate(url: &Url) -> Result<(), String> {
    if !matches!(url.scheme(), "https" | "http")
        || !url.has_host()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.as_str().len() > MAX_URL_LENGTH
    {
        return Err("Use an HTTP(S) address without embedded credentials.".into());
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceKind {
    Document,
    Stylesheet,
    Image,
    Script,
    Font,
}

impl ResourceKind {
    fn limit(self) -> usize {
        match self {
            Self::Document => MAX_DOCUMENT_BYTES,
            Self::Stylesheet => MAX_STYLESHEET_BYTES,
            Self::Image => MAX_IMAGE_BYTES,
            Self::Script => MAX_SCRIPT_BYTES,
            Self::Font => MAX_FONT_BYTES,
        }
    }

    fn accept(self) -> &'static str {
        match self {
            Self::Document => "text/html, application/xhtml+xml;q=0.9, text/plain;q=0.8",
            Self::Stylesheet => "text/css",
            Self::Image => "image/png, image/jpeg, image/webp, image/gif, image/svg+xml",
            Self::Script => {
                "text/javascript, application/javascript, text/typescript, application/typescript, text/plain;q=0.8"
            }
            Self::Font => {
                "font/woff2, font/woff, font/ttf, font/otf, application/font-woff, */*;q=0.8"
            }
        }
    }
}

pub struct Resource {
    pub url: Url,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

impl Resource {
    pub fn media_type(&self) -> &str {
        self.content_type.split(';').next().unwrap_or("").trim()
    }

    pub fn text(&self) -> String {
        let charset = self.content_type.split(';').skip(1).find_map(|value| {
            let (name, value) = value.trim().split_once('=')?;
            name.trim()
                .eq_ignore_ascii_case("charset")
                .then(|| value.trim().trim_matches(['\'', '"']))
        });
        let encoding = charset
            .and_then(|label| Encoding::for_label(label.as_bytes()))
            .unwrap_or(UTF_8);
        encoding.decode(&self.bytes).0.into_owned()
    }
}

#[derive(Clone)]
pub struct NetworkStack {
    client: Client,
    public_assets: Client,
}

impl NetworkStack {
    pub fn new() -> Result<Self, String> {
        let build = |builder: ClientBuilder| {
            builder
                .user_agent("Browser/0.2")
                .redirect(Policy::none())
                .connect_timeout(Duration::from_secs(4))
                .timeout(Duration::from_secs(8))
                .pool_max_idle_per_host(4)
                .build()
                .map_err(|error| format!("Could not initialize HTTP. {error}"))
        };
        Ok(Self {
            client: build(Client::builder())?,
            public_assets: build(
                Client::builder()
                    .no_proxy()
                    .dns_resolver(Arc::new(PublicDns)),
            )?,
        })
    }

    pub fn document(&self, url: &Url) -> Result<Resource, String> {
        self.fetch(
            url,
            ResourceKind::Document,
            None,
            Instant::now() + Duration::from_secs(8),
            ResourceKind::Document.accept(),
            None,
        )
    }

    /// Prefer a public HTML representation for a document that returned a script shell.
    pub fn html_document(
        &self,
        url: &Url,
        language: Option<&str>,
        deadline: Instant,
    ) -> Result<Resource, String> {
        self.fetch(
            url,
            ResourceKind::Document,
            None,
            deadline,
            "text/html",
            language,
        )
    }

    pub fn asset(
        &self,
        document: &Url,
        reference: &str,
        kind: ResourceKind,
        deadline: Instant,
    ) -> Result<Resource, String> {
        let url = document
            .join(reference.trim())
            .map_err(|_| "Invalid resource address.")?;
        self.fetch(&url, kind, Some(document), deadline, kind.accept(), None)
    }

    fn fetch(
        &self,
        url: &Url,
        kind: ResourceKind,
        owner: Option<&Url>,
        deadline: Instant,
        accept: &str,
        language: Option<&str>,
    ) -> Result<Resource, String> {
        let mut url = url.clone();
        url.set_fragment(None);
        let public_assets = owner.is_some_and(|document| !local_host(document));
        let client = if public_assets {
            &self.public_assets
        } else {
            &self.client
        };
        for _ in 0..=MAX_REDIRECTS {
            validate(&url)?;
            if let Some(owner) = owner
                && ((public_assets && local_host(&url))
                    || (owner.origin() != url.origin()
                        && (!public_assets || url.scheme() != "https")))
            {
                return Err("The document's resource is outside its allowed origins.".into());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("The site took too long to respond.".into());
            }
            let mut request = client.get(url.clone()).header(ACCEPT, accept);
            if let Some(language) = language {
                request = request.header(ACCEPT_LANGUAGE, language);
            }
            let response = request
                .timeout(remaining)
                .send()
                .map_err(|error| request_error(&error))?;
            let status = response.status();
            if matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308) {
                let location = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or("The site sent an invalid redirect.")?;
                let target = url
                    .join(location)
                    .map_err(|_| "The site sent an invalid redirect.")?;
                if url.scheme() == "https" && target.scheme() == "http" {
                    return Err("The site redirected from HTTPS to HTTP. Enter the HTTP address explicitly to open it.".into());
                }
                url = target;
                url.set_fragment(None);
                continue;
            }
            if !status.is_success() {
                return Err(format!("The server responded with {status}."));
            }
            return read_response(response, url, kind.limit());
        }
        Err("The site redirected too many times.".into())
    }
}

struct PublicDns;

impl Resolve for PublicDns {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let addresses = tokio::net::lookup_host((name.as_str(), 0)).await?;
            let addresses: Vec<_> = addresses
                .filter(|address| public_ip(address.ip()))
                .collect();
            if addresses.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "No public resource address.",
                )
                .into());
            }
            Ok(Box::new(addresses.into_iter()) as Addrs)
        })
    }
}

fn local_host(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(host)) => {
            let host = host.trim_end_matches('.');
            host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local")
        }
        Some(url::Host::Ipv4(address)) => !public_ip(IpAddr::V4(address)),
        Some(url::Host::Ipv6(address)) => !public_ip(IpAddr::V6(address)),
        None => true,
    }
}

fn public_ip(address: IpAddr) -> bool {
    match address.to_canonical() {
        IpAddr::V4(address) => {
            let [a, b, c, _] = address.octets();
            a > 0
                && a < 224
                && !address.is_private()
                && !address.is_loopback()
                && !address.is_link_local()
                && !address.is_documentation()
                && !(a == 100 && (64..128).contains(&b))
                && !(a == 192 && ((b == 0 && c == 0) || (b == 88 && c == 99)))
                && !(a == 198 && (b == 18 || b == 19))
        }
        IpAddr::V6(address) => {
            let [a, b, ..] = address.segments();
            (a & 0xe000) == 0x2000
                && !(a == 0x2001 && (b < 0x0200 || b == 0x0db8))
                && a != 0x2002
                && a != 0x3fff
        }
    }
}

fn request_error(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "The site took too long to respond.".into()
    } else if error.is_connect() {
        "Could not connect to the site. Check the address and HTTPS certificate.".into()
    } else {
        "Could not load the site's response.".into()
    }
}

fn read_response(response: Response, url: Url, limit: usize) -> Result<Resource, String> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err("The site's response exceeds the size limit.".into());
    }
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mut bytes = Vec::new();
    response
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "The site stopped sending data.")?;
    if bytes.len() > limit {
        return Err("The site's response exceeds the size limit.".into());
    }
    Ok(Resource {
        url,
        content_type,
        bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_domains_and_rejects_non_web_addresses() {
        assert_eq!(
            address("Example.COM").unwrap().as_str(),
            "https://example.com/"
        );
        assert_eq!(
            address("http://localhost:8080/page").unwrap().port(),
            Some(8080)
        );
        for input in [
            "file:///C:/private.html",
            "https://user:password@example.com",
            "two words",
        ] {
            assert!(address(input).is_err());
        }
    }

    #[test]
    fn public_asset_resolution_excludes_private_and_reserved_destinations() {
        for input in [
            "127.0.0.1",
            "10.0.0.7",
            "100.64.1.2",
            "169.254.169.254",
            "::ffff:127.0.0.1",
            "fe80::2",
            "fc00::4",
            "2001:db8::1",
        ] {
            assert!(!public_ip(input.parse().unwrap()));
        }
        for input in ["8.8.8.8", "2606:4700::1111"] {
            assert!(public_ip(input.parse().unwrap()));
        }
        assert!(local_host(&address("http://localhost:8787").unwrap()));
    }
}
