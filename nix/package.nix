{
  lib,
  stdenv,
  craneLib,
  makeWrapper,
  pkg-config,
  zlib,
  libiconv,
  git,
}:
let
  workspace = (lib.importTOML ../Cargo.toml).workspace.package;
  app = (lib.importTOML ../apps/hjkl/Cargo.toml).package;

  # The crates compile in TOML catalogs, tree-sitter queries and splash art
  # with include_str!, so the member trees go in whole: crane's
  # cleanCargoSource keeps only Rust and Cargo files and the build would fail
  # on the first missing asset.
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../apps
      ../crates
    ];
  };

  commonArgs = {
    pname = app.name;
    inherit (workspace) version;
    inherit src;
    strictDeps = true;
    cargoExtraArgs = "--locked -p ${app.name}";

    # libgit2-sys builds its bundled libgit2 against the system zlib, and
    # links iconv on macOS.
    nativeBuildInputs = [ pkg-config ];
    buildInputs = [ zlib ] ++ lib.optionals stdenv.hostPlatform.isDarwin [ libiconv ];

    # CI's test job runs the suite on every OS, and parts of it need a
    # terminal and network access that the build sandbox does not have.
    doCheck = false;
  };
in
craneLib.buildPackage (
  commonArgs
  // {
    cargoArtifacts = craneLib.buildDepsOnly commonArgs;

    nativeBuildInputs = commonArgs.nativeBuildInputs ++ [ makeWrapper ];

    # hjkl runs git for its pickers, hunk staging and the grammar fetcher, and
    # compiles tree-sitter grammars with cc / c++ on first use. Suffixed, so
    # the user's own git, compiler and $CC / $CXX still take precedence.
    postInstall = ''
      wrapProgram $out/bin/${app.name} \
        --suffix PATH : ${
          lib.makeBinPath [
            git
            stdenv.cc
          ]
        }
    '';

    meta = {
      inherit (app) description;
      inherit (workspace) homepage;
      license = lib.licenses.mit;
      mainProgram = app.name;
    };
  }
)
