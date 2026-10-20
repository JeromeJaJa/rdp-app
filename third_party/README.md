# Third-party code with local fixes

## ironrdp-graphics

Copy of `ironrdp-graphics` from IronRDP rev `62246b364a0282bab85087f658a81fe4207b6b31`
(MIT OR Apache-2.0, Copyright Devolutions), used through `[patch]` in the root `Cargo.toml`.

Local changes:

- `src/srl.rs`: SRL streams in RemoteFX Progressive upgrade passes are decoded as sent.
  Windows servers do not end them with the zero byte MS-RDPEGFX describes, so requiring
  (and stripping) one rejected almost every upgrade tile (`MissingTerminator`) or cut off
  real data (`Truncated`).

## ironrdp-rdpdr-native

Copy of `ironrdp-rdpdr-native` from IronRDP rev `62246b364a0282bab85087f658a81fe4207b6b31`
(MIT OR Apache-2.0, Copyright Devolutions and Teleport), used through `[patch]` in the root `Cargo.toml`.

Local changes:

- `src/windows/handles.rs` (`volume_root_nt_path`): a redirected drive can be rooted at any folder on a
  local volume (`D:\Work\Shared`), not only at a volume root (`C:\`). Upstream rejected folders with
  "redirected drive must be a logical volume root", which failed the whole session. Every open is
  still relative to the root handle, and folder roots may not contain `.`/`..` or empty components.
