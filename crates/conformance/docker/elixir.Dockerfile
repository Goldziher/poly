# Reference formatter: `mix format`, Elixir's canonical opinionated formatter.
# `mix format -` reads source on stdin and writes the formatted result to stdout.
FROM elixir:1.20.5-otp-29-alpine
ENTRYPOINT ["mix", "format", "-"]
