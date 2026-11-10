//! The paths of standing grants and never-lists (Settings › Sandbox, "Always allow" and "Never"
//! on a file request): checked and normalized before they are stored.
//!
//! A path is absolute (`/…`) or home-relative (`~`, `~/…`; each host expands `~` to its own
//! home), with no `.` or `..` components and no control characters. Repeated and trailing
//! slashes are dropped, so one path is stored one way. `/` can't be granted read-write.

use controlroom_protocol::RpcError;
use controlroom_protocol::session::{GrantKind, PathAccess};
use controlroom_protocol::ui::{NewGrant, ProfileChange};

use crate::errors::invalid_params;

/// `value` normalized, or why it isn't a usable path.
pub(crate) fn normalize(value: &str) -> Result<String, RpcError> {
    let value = value.trim();
    let (prefix, rest) = if value == "~" {
        ("~", "")
    } else if let Some(rest) = value.strip_prefix("~/") {
        ("~", rest)
    } else if let Some(rest) = value.strip_prefix('/') {
        ("", rest)
    } else {
        return Err(invalid_params(format!(
            "{value:?} is not absolute: start it with / or ~/"
        )));
    };
    if value.chars().any(char::is_control) {
        return Err(invalid_params("a path can't contain control characters"));
    }
    let mut out = prefix.to_owned();
    for part in rest.split('/').filter(|p| !p.is_empty()) {
        if part == "." || part == ".." {
            return Err(invalid_params(format!(
                "{value:?} contains . or ..; use the plain path"
            )));
        }
        out.push('/');
        out.push_str(part);
    }
    if out.is_empty() {
        out.push('/');
    }
    Ok(out)
}

/// `change` with its path checked and normalized; other changes as they are.
pub(crate) fn check_change(change: ProfileChange) -> Result<ProfileChange, RpcError> {
    Ok(match change {
        ProfileChange::AddGrant { grant } => ProfileChange::AddGrant {
            grant: check_grant(grant)?,
        },
        ProfileChange::AddNever { path } => ProfileChange::AddNever {
            path: normalize(&path)?,
        },
        other => other,
    })
}

fn check_grant(grant: NewGrant) -> Result<NewGrant, RpcError> {
    if grant.kind != GrantKind::Path {
        return Err(invalid_params(
            "only path grants are added this way (domains have add_domain)",
        ));
    }
    let access = match grant.access {
        Some(access @ (PathAccess::Read | PathAccess::ReadWrite)) => access,
        _ => {
            return Err(invalid_params(
                "a path grant's access is read or read_write",
            ));
        }
    };
    let value = normalize(&grant.value)?;
    if value == "/" && access == PathAccess::ReadWrite {
        return Err(invalid_params("/ can't be granted read-write"));
    }
    Ok(NewGrant {
        kind: GrantKind::Path,
        value,
        access: Some(access),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_absolute_or_home_relative_and_normalized() {
        let ok = |s: &str| normalize(s).unwrap();
        assert_eq!(ok("/home//alice/.cache/"), "/home/alice/.cache");
        assert_eq!(ok(" ~/.cache/nix "), "~/.cache/nix");
        assert_eq!(ok("~"), "~");
        assert_eq!(ok("~/"), "~");
        assert_eq!(ok("/"), "/");
        for bad in [
            "relative", "./x", "~alice/x", "/a/../b", "/a/./b", "~/..", "", "/a\nb",
        ] {
            assert!(normalize(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn grants_are_paths_with_read_or_read_write_and_root_stays_read_only() {
        let grant = |kind, value: &str, access| {
            check_change(ProfileChange::AddGrant {
                grant: NewGrant {
                    kind,
                    value: value.into(),
                    access,
                },
            })
        };
        let path = GrantKind::Path;
        assert_eq!(
            grant(path, "~/.cache//nix/", Some(PathAccess::ReadWrite)).unwrap(),
            ProfileChange::AddGrant {
                grant: NewGrant {
                    kind: path,
                    value: "~/.cache/nix".into(),
                    access: Some(PathAccess::ReadWrite),
                },
            }
        );
        assert!(grant(path, "/", Some(PathAccess::Read)).is_ok());
        assert!(grant(path, "/", Some(PathAccess::ReadWrite)).is_err());
        assert!(grant(path, "/srv", Some(PathAccess::Write)).is_err());
        assert!(grant(path, "/srv", None).is_err());
        assert!(grant(GrantKind::Domain, "example.com", None).is_err());
        assert!(check_change(ProfileChange::AddNever { path: "rel".into() }).is_err());
    }
}
