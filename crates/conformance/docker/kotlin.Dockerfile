# Reference formatter: ktfmt (Meta) — opinionated Kotlin formatter. ktfmt
# formats files in place, so the entrypoint buffers stdin to a temp file,
# formats it, and writes the result to stdout.
FROM eclipse-temurin:21-jre-alpine
ADD https://repo1.maven.org/maven2/com/facebook/ktfmt/0.54/ktfmt-0.54-jar-with-dependencies.jar /opt/ktfmt.jar
RUN printf '#!/bin/sh\nset -e\ncat > /tmp/In.kt\njava -jar /opt/ktfmt.jar /tmp/In.kt >/dev/null 2>&1 || true\ncat /tmp/In.kt\n' \
    > /usr/local/bin/fmt && chmod +x /usr/local/bin/fmt
ENTRYPOINT ["/usr/local/bin/fmt"]
