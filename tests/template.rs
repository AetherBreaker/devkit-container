//! The shipped Dockerfile template's shape (timeclock design §11.1).

fn template() -> String {
  std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/python/devkit_container/template.Dockerfile")).unwrap()
}

#[test]
fn both_stages_build_on_trixie() {
  let t = template();
  let froms: Vec<&str> = t.lines().filter(|l| l.starts_with("FROM ")).collect();
  assert_eq!(
    froms,
    [
      "FROM ghcr.io/astral-sh/uv:python3.14-trixie-slim AS builder",
      "FROM ghcr.io/astral-sh/uv:python3.14-trixie-slim",
    ]
  );
  assert!(!t.contains("bookworm"));
}

#[test]
fn builder_setup_window_sits_between_the_git_install_and_the_clone() {
  let t = template();
  let at = |needle: &str| {
    t.lines()
      .position(|l| l.starts_with(needle))
      .unwrap_or_else(|| panic!("{needle:?} missing"))
  };
  let git = at("RUN apt-get update && apt-get install -y --no-install-recommends git");
  let open = at("# !window builder-setup:");
  let close = at("# !end builder-setup");
  let clone = at("RUN git clone ");
  let sync = at("RUN --mount=type=cache");
  assert!(
    git < open && open < close && close < clone && clone < sync,
    "{git} {open} {close} {clone} {sync}"
  );
  assert_eq!(close, open + 1, "the window ships empty");
}

#[test]
fn builder_cached_window_sits_above_the_git_args() {
  let t = template();
  let at = |needle: &str| {
    t.lines()
      .position(|l| l.starts_with(needle))
      .unwrap_or_else(|| panic!("{needle:?} missing"))
  };
  let workdir = at("WORKDIR /app");
  let open = at("# !window builder-cached:");
  let close = at("# !end builder-cached");
  let arg = at("ARG GIT_TAG");
  assert!(workdir < open && open < close && close < arg, "{workdir} {open} {close} {arg}");
  assert_eq!(close, open + 1, "the window ships empty");
}
