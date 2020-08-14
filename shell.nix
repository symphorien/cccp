with import <nixpkgs> {};
mkShell {
  buildInputs = [ cargo rls rustfmt rustc clippy ];
}
