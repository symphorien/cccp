with import <nixpkgs> {};
mkShell {
  buildInputs = [ cargo rls rustfmt rustc clippy ];
  RUSTUP_TOOLCHAIN = "stable";
}
