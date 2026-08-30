# @goldziher/polylint-linux-x64-musl

The prebuilt `poly` binary for **Linux x64 (musl)** (`x86_64-unknown-linux-musl`).

This package exists only so that [`@goldziher/polylint`](https://www.npmjs.com/package/@goldziher/polylint)
can pull exactly one platform binary through `optionalDependencies`. Install that
package instead — it is what puts the `poly` command on your `PATH`:

```sh
npm install --global @goldziher/polylint
```
