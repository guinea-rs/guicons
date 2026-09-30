/// A manifest key as a Rust `SCREAMING_SNAKE_CASE` constant name, prefixed
/// with `ICON_` when it would otherwise start with a digit.
pub fn rust_const_name(key: &str) -> String {
    let name = key.replace(['.', '-'], "_").to_ascii_uppercase();
    if name.is_empty() || name.starts_with(|ch: char| ch.is_ascii_digit()) {
        format!("ICON_{name}")
    } else {
        name
    }
}

/// A manifest key (`settings-filled`) as a Rust/Slint `UpperCamelCase`
/// type-name fragment - used for both `guicons-build`'s generated Rust
/// builder struct names and its generated Slint component names (e.g.
/// `SettingsFilledIcon`), so it lives here rather than in either codegen
/// module - anything that needs to predict a name codegen will produce
/// (an IDE feature suggesting `icon!`-adjacent Slint syntax, say) can
/// call the exact same function instead of keeping its own copy in sync
/// by hand.
pub fn rust_variant_name(key: &str) -> String {
    let mut result = String::new();
    for segment in key.split(['.', '-', '_']) {
        if segment.is_empty() {
            continue;
        }
        let mut chars = segment.chars();
        if let Some(first) = chars.next() {
            result.push(first.to_ascii_uppercase());
            result.push_str(chars.as_str());
        }
    }
    if result.is_empty() {
        "Unknown".to_string()
    } else if result.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        format!("Icon{result}")
    } else {
        result
    }
}

/// Converts a manifest name (a family or variant) into a Rust `snake_case`
/// fn/method name, guarding against the two ways an otherwise valid segment
/// can fail to be a valid identifier: starting with a digit (`20` ->
/// `icon_20`) and colliding with a reserved keyword (`type` -> `r#type`).
pub fn rust_fn_name(name: &str) -> String {
    let mut result = String::new();
    for segment in name.split(['.', '-', '_']) {
        if segment.is_empty() {
            continue;
        }
        if !result.is_empty() {
            result.push('_');
        }
        result.push_str(&segment.to_ascii_lowercase());
    }
    if result.is_empty() {
        return "icon".to_string();
    }
    if result.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        result = format!("icon_{result}");
    }
    if is_rust_keyword(&result) {
        result = format!("r#{result}");
    }
    result
}

/// `syn::parse_str::<syn::Ident>` already rejects every reserved/strict
/// keyword (and returns `Err` rather than panicking, unlike
/// `proc_macro2::Ident::new`) - no need to maintain our own copy of that list.
fn is_rust_keyword(name: &str) -> bool {
    syn::parse_str::<syn::Ident>(name).is_err()
}

#[cfg(test)]
mod tests {
    use super::{rust_const_name, rust_fn_name};

    #[test]
    fn const_name_never_starts_with_a_digit() {
        assert_eq!(rust_const_name("3d-cube"), "ICON_3D_CUBE");
        assert_eq!(rust_const_name("settings-filled"), "SETTINGS_FILLED");
    }

    #[test]
    fn converts_dashes_to_snake_case() {
        assert_eq!(rust_fn_name("group-list"), "group_list");
    }

    #[test]
    fn lowercases_mixed_case_segments() {
        assert_eq!(rust_fn_name("Settings"), "settings");
    }

    #[test]
    fn prefixes_a_leading_digit() {
        assert_eq!(rust_fn_name("20"), "icon_20");
    }

    #[test]
    fn escapes_a_reserved_keyword() {
        assert_eq!(rust_fn_name("type"), "r#type");
    }

    #[test]
    fn falls_back_to_icon_for_an_empty_name() {
        assert_eq!(rust_fn_name(""), "icon");
    }
}
