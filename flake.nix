{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs =
    { nixpkgs, ... }:
    let
      forAllSystems = nixpkgs.lib.genAttrs [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
    in
    {
      devShells = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          # libghostty-vt-sys builds Ghostty from source and requires this exact Zig
          default = pkgs.mkShell {
            packages = [
              pkgs.zig_0_15
              pkgs.cargo-nextest
            ];
          };
        }
      );
    };
}
