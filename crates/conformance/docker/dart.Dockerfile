# Reference formatter: `dart format` — the canonical Dart formatter shipped with
# the Dart SDK. `-o show` writes the formatted result to stdout without
# modifying the input in place.
FROM dart:stable
RUN printf '#!/bin/sh\nset -e\ncat > /tmp/in.dart\ndart format -o show /tmp/in.dart\n' \
    > /usr/local/bin/fmt && chmod +x /usr/local/bin/fmt
ENTRYPOINT ["/usr/local/bin/fmt"]
