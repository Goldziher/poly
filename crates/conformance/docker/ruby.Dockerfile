# Reference formatter: standardrb (Standard Ruby) — opinionated, zero-config.
# standardrb autocorrects files in place, so the entrypoint buffers stdin to a
# temp file, fixes it, and writes the result to stdout.
FROM ruby:3.3-alpine
# standard pulls gems with native extensions (prism, etc.); needs a toolchain.
RUN apk add --no-cache build-base && gem install standard --no-document
RUN printf '#!/bin/sh\nset -e\ncat > /tmp/in.rb\nstandardrb --fix /tmp/in.rb >/dev/null 2>&1 || true\ncat /tmp/in.rb\n' \
    > /usr/local/bin/fmt && chmod +x /usr/local/bin/fmt
ENTRYPOINT ["/usr/local/bin/fmt"]
