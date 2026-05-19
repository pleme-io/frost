# frost :: top-level module re-exports
#
# Single import point for consumers. `import ./module` returns an
# attrset with one module per platform; the flake re-exports them
# as `homeManagerModules.default`, `nixosModules.default`,
# `darwinModules.default`.
{
  homeManager = ./home-manager;
  nixos = ./nixos;
  darwin = ./darwin;
}
