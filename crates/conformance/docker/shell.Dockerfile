# Reference formatter: shfmt (mvdan). Reads stdin, writes formatted shell to
# stdout. `-i 2` = two-space indent (matches the project's shfmt prek hook).
FROM mvdan/shfmt:v3.10.0-alpine
ENTRYPOINT ["/bin/shfmt", "-i", "2"]
