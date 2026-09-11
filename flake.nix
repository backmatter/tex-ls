{
  description = "A language server, formatter, and linter for LaTeX";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
        };

        meaning = pkgs.rustPlatform.buildRustPackage {
          pname = "meaning";
          version = "0.1.0";

          src = ./.;

          cargoLock = {
            lockFile = ./Cargo.lock;
            outputHashes = {
              "distro-0.0.0" = "sha256-qPPmCn9qAzZuMDJS8AM1EVT0LomrhNgPfhcL/PAIZ0k=";
            };
          };

          nativeBuildInputs = [ pkgs.installShellFiles ];

          postInstall = ''
            installShellCompletion --cmd meaning \
              --bash target/completions/meaning.bash \
              --fish target/completions/meaning.fish \
              --zsh target/completions/_meaning

            installManPage target/man/*
          '';

          meta = with pkgs.lib; {
            description = "A language server, formatter, and linter for LaTeX";
            homepage = "https://github.com/backmatter/meaning";
            license = licenses.mit;
            maintainers = [ ];
          };
        };
      in
      {
        packages = {
          default = meaning;
          meaning = meaning;
        };

        apps = {
          default = {
            type = "app";
            program = "${meaning}/bin/meaning";
          };
        };

        devShells.default = pkgs.mkShell {
          buildInputs = with pkgs; [
            cargo
            rustc
            rustfmt
            clippy
            rust-analyzer
            go-task
            wasm-pack
            llvmPackages.bintools
          ];
        };
      }
    );
}
