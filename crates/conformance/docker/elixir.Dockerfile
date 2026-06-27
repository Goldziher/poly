# Reference formatter: `mix format`, Elixir's canonical opinionated formatter.
# `mix format -` reads source on stdin and writes the formatted result to stdout.
FROM elixir:1.18.4-otp-27-alpine
ENTRYPOINT ["mix", "format", "-"]
