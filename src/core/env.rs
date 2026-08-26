//! Environment knobs for build-script policy flags.

/// Read a boolean policy flag from the environment.
///
/// Unset and empty both mean `default`: a Docker `ARG X=` passed through
/// `ENV X=${X}` yields an empty string, not an absent variable. `"1"` and
/// `"0"` are explicit; anything else panics naming the variable, so a typo
/// fails the build instead of silently picking a default.
///
/// ```no_run
/// let minify = web_modules::env::flag("MY_MINIFY", cfg!(not(debug_assertions)));
/// let output = web_modules::build::Output::new(minify, false);
/// # let _ = output;
/// ```
pub fn flag(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Err(std::env::VarError::NotPresent) => default,
        Ok(value) if value.is_empty() => default,
        Ok(value) if value == "1" => true,
        Ok(value) if value == "0" => false,
        Ok(value) => panic!("{name} must be \"1\", \"0\" or unset, got {value:?}"),
        Err(e) => panic!("{name}: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::flag;

    // Each test owns a unique variable name, so the process-global
    // environment never races across parallel tests.

    #[test]
    fn unset_and_empty_mean_default() {
        assert!(flag("WEB_MODULES_TEST_FLAG_UNSET", true));
        assert!(!flag("WEB_MODULES_TEST_FLAG_UNSET", false));
        std::env::set_var("WEB_MODULES_TEST_FLAG_EMPTY", "");
        assert!(flag("WEB_MODULES_TEST_FLAG_EMPTY", true));
        assert!(!flag("WEB_MODULES_TEST_FLAG_EMPTY", false));
    }

    #[test]
    fn explicit_values_override_the_default() {
        std::env::set_var("WEB_MODULES_TEST_FLAG_ON", "1");
        assert!(flag("WEB_MODULES_TEST_FLAG_ON", false));
        std::env::set_var("WEB_MODULES_TEST_FLAG_OFF", "0");
        assert!(!flag("WEB_MODULES_TEST_FLAG_OFF", true));
    }

    #[test]
    #[should_panic(expected = "WEB_MODULES_TEST_FLAG_JUNK")]
    fn junk_panics_naming_the_variable() {
        std::env::set_var("WEB_MODULES_TEST_FLAG_JUNK", "yes");
        flag("WEB_MODULES_TEST_FLAG_JUNK", true);
    }
}
