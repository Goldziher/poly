# Reference formatter: Laravel Pint — opinionated, zero-config PHP formatter.
# Pint fixes files in place, so the entrypoint buffers stdin to a temp file,
# formats it, and writes the result to stdout.
FROM composer:2
ENV COMPOSER_HOME=/composer
RUN composer global require laravel/pint --no-interaction --quiet
RUN printf '#!/bin/sh\nset -e\ncat > /tmp/in.php\n/composer/vendor/bin/pint /tmp/in.php >/dev/null 2>&1 || true\ncat /tmp/in.php\n' \
    > /usr/local/bin/fmt && chmod +x /usr/local/bin/fmt
ENTRYPOINT ["/usr/local/bin/fmt"]
