//! `btp shell-init zsh|bash`: shell functions that send the dev server
//! commands typed in a terminal through `btp run`. The user adds
//! `eval "$(btp shell-init zsh)"` to their rc file; btp never edits it.
//! `btp _is-dev` is the cheap check the functions call first.

use btp_core::run::command::recognize;

/// Commands wrapped by the functions.
pub const WRAPPED: &[&str] = &[
    "npm", "pnpm", "yarn", "bun", "npx", "next", "vite", "astro", "wrangler", "expo", "nuxt",
    "remix", "eleventy",
];

/// Shared by zsh and bash: the syntax below is common to both. The wrapper
/// returns the real command's exit code and passes `"$@"` through; it goes
/// straight to the real command when `btp` is missing, when `BTP_RUN=0`,
/// when already inside `btp run`, or when `_is-dev` does not say yes.
const COMMON: &str = r#"export BTP_SHELL_WRAP=1
__btp_wrap() {
  local __btp_name="$1"
  shift
  if [ "${BTP_RUN_ACTIVE:-}" = 1 ] || [ "${BTP_RUN:-}" = 0 ] \
    || ! command -v btp >/dev/null 2>&1; then
    command "$__btp_name" "$@"
    return $?
  fi
  # No fallback to the real command when `btp run` itself fails: that would
  # start the server a second time.
  if command btp _is-dev -- "$__btp_name" "$@" >/dev/null 2>&1; then
    command btp run -- "$__btp_name" "$@"
  else
    command "$__btp_name" "$@"
  fi
}
"#;

/// The text to eval for `shell`, or `None` for an unknown shell.
pub fn snippet(shell: &str) -> Option<String> {
    if shell != "zsh" && shell != "bash" {
        return None;
    }
    let mut out = String::from(COMMON);
    for name in WRAPPED {
        // `function` form: unlike `name()`, it does not expand a user alias
        // of the same name, so an alias never breaks the eval (it keeps
        // winning at call time and the wrapper is just unused).
        out.push_str(&format!(
            "function {name} {{ __btp_wrap {name} \"$@\"; }}\n"
        ));
    }
    Some(out)
}

/// Exit code of `btp _is-dev -- <argv...>`: 0 when `argv` starts a known dev
/// server here, 1 otherwise. No state, no watcher, no session scan.
pub fn is_dev(argv: &[String]) -> u8 {
    let Ok(cwd) = std::env::current_dir() else {
        return 1;
    };
    let env_port = std::env::var("PORT").ok();
    u8::from(recognize(argv, &cwd, env_port.as_deref()).is_none())
}
