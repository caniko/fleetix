{rustfmtPackage, ...}: {
  projectRootFile = "flake.nix";
  programs.alejandra.enable = true;
  programs.rustfmt = {
    enable = true;
    package = rustfmtPackage;
    edition = "2021";
  };
  # Match the existing fleet-formatted source while parsing its Cargo edition.
  settings.formatter.rustfmt.options = ["--style-edition=2024"];
}
