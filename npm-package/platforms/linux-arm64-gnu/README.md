# @goldziher/polylint-linux-arm64-gnu

The prebuilt `poly` binary for **Linux arm64 (glibc)** (`aarch64-unknown-linux-gnu`).

This package exists only so that [`@goldziher/polylint`](https://www.npmjs.com/package/@goldziher/polylint)
can pull exactly one platform binary through `optionalDependencies`. Install that
package instead — it is what puts the `poly` command on your `PATH`:

```sh
npm install --global @goldziher/polylint
```
