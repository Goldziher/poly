# Reference formatter: CSharpier — the opinionated, Prettier-style C# formatter.
# Installed as a global dotnet tool. CSharpier formats files in place, so the
# entrypoint buffers stdin to a temp file, formats it, and cats the result.
FROM mcr.microsoft.com/dotnet/sdk:8.0
RUN dotnet tool install -g csharpier \
    && ln -s /root/.dotnet/tools/csharpier /usr/local/bin/csharpier
RUN printf '#!/bin/sh\nset -e\ncat > /tmp/In.cs\ncsharpier format /tmp/In.cs >/dev/null 2>&1 || true\ncat /tmp/In.cs\n' \
    > /usr/local/bin/fmt && chmod +x /usr/local/bin/fmt
ENTRYPOINT ["/usr/local/bin/fmt"]
