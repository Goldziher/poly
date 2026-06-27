# Reference formatter: `zig fmt` — the canonical Zig formatter bundled with the
# compiler. Installed from the pinned static release tarball (no official slim
# image). `zig fmt` formats in place, so the entrypoint buffers, formats, cats.
FROM alpine:3.20
ARG ZIG_VERSION=0.13.0
RUN apk add --no-cache wget xz \
    && wget -q "https://ziglang.org/download/${ZIG_VERSION}/zig-linux-$(uname -m)-${ZIG_VERSION}.tar.xz" -O /tmp/zig.tar.xz \
    && mkdir -p /opt/zig && tar -xf /tmp/zig.tar.xz -C /opt/zig --strip-components=1 \
    && ln -s /opt/zig/zig /usr/local/bin/zig && rm /tmp/zig.tar.xz
RUN printf '#!/bin/sh\nset -e\ncat > /tmp/in.zig\nzig fmt /tmp/in.zig >/dev/null 2>&1 || true\ncat /tmp/in.zig\n' \
    > /usr/local/bin/fmt && chmod +x /usr/local/bin/fmt
ENTRYPOINT ["/usr/local/bin/fmt"]
