with import <nixpkgs> {};
mkShell {
  buildInputs = [ cargo rls rustc clippy ];
}
