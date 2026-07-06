// Known-bad JS: triggers the oxlint `no-debugger` rule.
export function processData(data) {
  const result = data.map((x) => x * 2);
  debugger;
  return result;
}
