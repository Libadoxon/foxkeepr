{ pkgs, ... }: {
  projectRootFile = ".gitignore";

  programs.rustfmt.enable = true;
  programs.rustfmt.edition = "2024";
  programs.taplo.enable = true;
  programs.taplo.settings = {
    formatting.reorder_keys = false;
    rule = [
      {
        include = [ "**/Cargo.toml" ];
        keys = [ "dependencies" ];
        formatting.reorder_keys = true;
      }
    ];
  };
  programs.nixfmt.enable = true;
  programs.nixfmt.strict = true;
}
