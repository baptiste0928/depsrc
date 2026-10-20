{
  description = "Fetch dependency sources with their git history, for agents to read";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs =
    { self, nixpkgs }:
    let
      forAllSystems = nixpkgs.lib.genAttrs [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          # git and tar are run as commands, tar runs gzip for `-z`.
          runtime = [
            pkgs.gitMinimal
            pkgs.gnutar
            pkgs.gzip
          ];
        in
        {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "depsrc";
            version = (pkgs.lib.importTOML ./Cargo.toml).package.version;
            src = self;
            cargoHash = "sha256-8+PnOHxVgxn+QqnJryxU6jqTRbl1hFpD9p0IqeEuIpM=";

            nativeBuildInputs = [
              pkgs.makeWrapper
              pkgs.installShellFiles
            ];
            nativeCheckInputs = runtime;

            # The user's PATH comes after, for ssh and git helpers.
            postInstall = ''
              wrapProgram $out/bin/depsrc --prefix PATH : ${pkgs.lib.makeBinPath runtime}
            ''
            # Completions call back the installed binary.
            + pkgs.lib.optionalString (pkgs.stdenv.buildPlatform.canExecute pkgs.stdenv.hostPlatform) ''
              installShellCompletion --cmd depsrc \
                --bash <(COMPLETE=bash $out/bin/depsrc) \
                --zsh <(COMPLETE=zsh $out/bin/depsrc) \
                --fish <(COMPLETE=fish $out/bin/depsrc)
            '';

            meta.mainProgram = "depsrc";
          };
        }
      );
    };
}
