# @goldziher/polylint-darwin-arm64

The prebuilt `poly` binary for **macOS (Apple silicon)** (`aarch64-apple-darwin`).

This package exists only so that [`@goldziher/polylint`](https://www.npmjs.com/package/@goldziher/polylint)
can pull exactly one platform binary through `optionalDependencies`. Install that
package instead — it is what puts the `poly` command on your `PATH`:

```sh
npm install --global @goldziher/polylint
```
