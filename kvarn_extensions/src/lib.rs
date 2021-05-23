//! # Kvarn extensions
//! A *supporter-lib* for Kvarn to supply extensions to the web server.
//!
//! Use [`new()`] to get started quickly.
//!
//! ## An introduction to the *Kvarn extension system*
//! On of the many things Kvarn extensions can to is bind to *extension declarations* and to *file extensions*.
//! For example, if you mount the extensions [`download`], it binds the *extension declaration* `download`.
//! If you then, in a file inside your `public/` directory, add `!> download` to the top, the client visiting the url pointing to the file will download it.

use kvarn::{extensions::*, prelude::*};

#[cfg(feature = "reverse-proxy")]
#[path = "reverse-proxy.rs"]
pub mod reverse_proxy;
#[cfg(feature = "reverse-proxy")]
pub use reverse_proxy::{
    localhost, static_connection, Connection as ReverseProxyConnection, Manager as ReverseProxy,
};

#[cfg(feature = "push")]
pub mod push;
#[cfg(feature = "push")]
pub use push::push;

#[cfg(feature = "fastcgi-client")]
pub mod fastcgi;

#[cfg(feature = "php")]
pub mod php;
#[cfg(feature = "php")]
pub use php::php;

#[cfg(feature = "templates")]
pub mod templates;
#[cfg(feature = "templates")]
pub use templates::templates;

/// Creates a new `Extensions` and adds all enabled `kvarn_extensions`.
///
/// See [`mount_all()`] for more information.
pub fn new() -> Extensions {
    let mut e = Extensions::new();
    mount_all(&mut e);
    e
}

/// Mounts all extensions specified in Cargo.toml dependency declaration.
///
/// The current defaults are [`download()`], [`cache()`], [`php()`], and [`templates()`]
///
/// They will *always* get included in your server after calling this function.
pub fn mount_all(extensions: &mut Extensions) {
    extensions.add_present_internal("download".to_string(), Box::new(download));
    extensions.add_present_internal("cache".to_string(), Box::new(cache));
    extensions.add_present_internal("hide".to_string(), Box::new(hide));
    extensions.add_present_file("private".to_string(), Box::new(hide));
    extensions.add_present_internal("allow-ips".to_string(), Box::new(ip_allow));
    #[cfg(feature = "php")]
    extensions.add_prepare_fn(
        Box::new(|req| req.uri().path().ends_with(".php")),
        Box::new(php),
    );
    #[cfg(feature = "templates")]
    extensions.add_present_internal("tmpl".to_string(), Box::new(templates));
    #[cfg(feature = "push")]
    extensions.add_post(Box::new(push));
}

// Ok, since it is used, just not by every extension, and #[CFG] would be too fragile for this.
#[allow(dead_code)]
pub mod parse {
    use super::*;

    pub fn format_file_name<P: AsRef<Path>>(path: &P) -> Option<&str> {
        path.as_ref().file_name().and_then(std::ffi::OsStr::to_str)
    }
    pub fn format_file_path<P: AsRef<Path>>(path: &P) -> Result<PathBuf, io::Error> {
        let mut file_path = std::env::current_dir()?;
        file_path.push(path);
        Ok(file_path)
    }
}

#[cfg(feature = "templates")]

/// Makes the client download the file.
pub fn download(mut data: PresentDataWrapper) -> RetFut<()> {
    let data = unsafe { data.get_inner() };
    let headers = data.response_mut().headers_mut();
    kvarn::utility::replace_header_static(headers, "content-type", "application/octet-stream");
    ready(())
}

pub fn cache(mut data: PresentDataWrapper) -> RetFut<()> {
    fn parse<'a, I: Iterator<Item = &'a str>>(
        iter: I,
    ) -> (Option<ClientCachePreference>, Option<ServerCachePreference>) {
        let mut c = None;
        let mut s = None;
        for arg in iter {
            let mut parts = arg.split(':');
            let domain = parts.next();
            let cache = parts.next();
            if let (Some(domain), Some(cache)) = (domain, cache) {
                match domain {
                    "client" => {
                        if let Ok(preference) = cache.parse() {
                            c = Some(preference)
                        }
                    }
                    "server" => {
                        if let Ok(preference) = cache.parse() {
                            s = Some(preference)
                        }
                    }
                    _ => {}
                }
            }
        }
        (c, s)
    }
    let data = unsafe { data.get_inner() };
    let preference = parse(data.args().iter());
    if let Some(c) = preference.0 {
        *data.client_cache_preference() = c;
    }
    if let Some(s) = preference.1 {
        *data.server_cache_preference() = s;
    }
    ready(())
}

pub fn hide(mut data: PresentDataWrapper) -> RetFut<()> {
    box_fut!({
        let data = unsafe { data.get_inner() };
        let error = default_error(StatusCode::NOT_FOUND, Some(data.host()), None).await;
        *data.response_mut() = error;
    })
}

pub fn ip_allow(mut data: PresentDataWrapper) -> RetFut<()> {
    box_fut!({
        let data = unsafe { data.get_inner() };
        let mut matched = false;
        // Loop over denied ip in args
        for denied in data.args().iter() {
            // If parsed
            if let Ok(ip) = denied.parse::<IpAddr>() {
                // check it against the requests IP.
                if data.address().ip() == ip {
                    matched = true;
                    // Then break out of loop
                    break;
                }
            }
        }
        *data.server_cache_preference() = kvarn::comprash::ServerCachePreference::None;
        *data.client_cache_preference() = kvarn::comprash::ClientCachePreference::Changing;

        if !matched {
            // If it does not match, set the response to 404
            let error = default_error(StatusCode::NOT_FOUND, Some(data.host()), None).await;
            *data.response_mut() = error;
        }
    })
}
