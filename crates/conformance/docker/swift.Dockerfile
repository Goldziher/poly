# Reference formatter: swift-format (Apple) — the canonical Swift formatter,
# bundled with the Swift 6 toolchain. It formats a file to stdout, so the
# entrypoint buffers stdin to a temp file and formats that.
FROM swift:6.0-jammy
RUN printf '#!/bin/sh\nset -e\ncat > /tmp/in.swift\nswift-format format /tmp/in.swift\n' \
    > /usr/local/bin/fmt && chmod +x /usr/local/bin/fmt
ENTRYPOINT ["/usr/local/bin/fmt"]
