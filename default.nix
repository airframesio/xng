{ pkgs ? import <nixpkgs> {} }:
  pkgs.mkShell {
    packages = [
      pkgs.darwin.Security
      pkgs.pkg-config
      pkgs.iconv
      pkgs.openssl
      pkgs.soapysdr
    ];
  }
