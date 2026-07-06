// Known-bad JS fixture: triggers the oxlint `no-debugger` correctness rule.
// The function is exported so it is not flagged by `no-unused-vars`.
export function processData(data) {
  const result = data.map((x) => x * 2);
  debugger;
  return result;
}
