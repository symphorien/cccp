with import <nixpkgs> {};
mkShell {
  buildInputs = [ cargo rls rustfmt rustc clippy udev dbus pkg-config ];
  RUSTUP_TOOLCHAIN = "stable";
}
