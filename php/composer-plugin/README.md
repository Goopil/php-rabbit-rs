# RabbitRs Composer Plugin

Automates downloading the pre-built `rabbit_rs` PHP extension from GitHub
releases and drops a PHP stub file for editor autocompletion.

## Configuration

```json
{
  "require": {
    "goopil/rabbit-rs-installer": "^0.1"
  },
  "extra": {
    "rabbit-rs": {
      "version": "0.1.0",
      "download_template": "https://github.com/goopil/php-rabbit-rs/releases/download/%tag%/%file%"
    }
  }
}
```

`version` is required unless the `RABBIT_RS_VERSION` environment variable is
provided. It must match a Git tag (without the leading `v`).

## Environment variables

* `RABBIT_RS_VERSION`: override the release tag to fetch (ex: `v0.1.0`).
* `RABBIT_RS_FORCE_INSTALL`: set to `1` to force re-download even if a cached
  binary is present.

## Output

* Extension binaries are stored under `vendor/goopil/rabbit-rs/ext/<platform>/php<version>`.
* A PHP stub is copied to `vendor/goopil/rabbit-rs/stubs/RabbitRs.stub.php`.

