// The TextMate grammar: tokenize every example and check the scopes of a few known tokens.
// Uses the same tokenizer as VS Code (vscode-textmate + vscode-oniguruma).
import { readFileSync, readdirSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import vsctm from "vscode-textmate";
import oniguruma from "vscode-oniguruma";

const require = createRequire(import.meta.url);
const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..", "..", "..");
const wasm = readFileSync(join(dirname(require.resolve("vscode-oniguruma")), "onig.wasm"));
await oniguruma.loadWASM(wasm.buffer.slice(wasm.byteOffset, wasm.byteOffset + wasm.byteLength));

const registry = new vsctm.Registry({
  onigLib: Promise.resolve({
    createOnigScanner: (s) => new oniguruma.OnigScanner(s),
    createOnigString: (s) => new oniguruma.OnigString(s),
  }),
  loadGrammar: async (scope) =>
    scope === "source.wys"
      ? vsctm.parseRawGrammar(
          readFileSync(join(here, "..", "syntaxes", "wys.tmLanguage.json"), "utf8"),
          "wys.tmLanguage.json"
        )
      : null,
});
const grammar = await registry.loadGrammar("source.wys");

function tokenize(text) {
  const out = [];
  let stack = vsctm.INITIAL;
  for (const line of text.split("\n")) {
    const r = grammar.tokenizeLine(line, stack);
    for (const t of r.tokens) {
      out.push({ text: line.slice(t.startIndex, t.endIndex), scopes: t.scopes });
    }
    stack = r.ruleStack;
  }
  return out;
}

let failures = 0;
function expectScope(src, token, scope) {
  const hit = tokenize(src).find((t) => t.text === token);
  const scopes = hit ? hit.scopes.join(" ") : "(token not found)";
  if (!hit || !hit.scopes.some((s) => s.startsWith(scope))) {
    console.error(`FAIL ${JSON.stringify(token)} in ${JSON.stringify(src)}: want ${scope}, got ${scopes}`);
    failures++;
  }
}

expectScope("def main() -> None:", "def", "storage.type.function");
expectScope("def main() -> None:", "main", "entity.name.function");
expectScope("def main() -> None:", "None", "constant.language");
expectScope("type Point = {x: int, y: int}", "Point", "entity.name.type");
expectScope("type Point = {x: int, y: int}", "int", "support.type");
expectScope('@tool(description="x", approval="required")', "tool", "entity.name.function.decorator");
expectScope('    print("a # not a comment")', '"', "punctuation.definition.string.begin");
expectScope('    print("a # not a comment")', "a # not a comment", "string.quoted.double");
expectScope("    x = 1 # note", "# note", "comment.line");
expectScope("    s = 'it\\'s'", "\\'", "constant.character.escape");
expectScope("async def f() -> int uses io, llm:", "uses", "keyword.other.uses");
expectScope("    return await llm_generate[Summary](req)", "llm_generate", "entity.name.function.call");
expectScope("    match r:", "match", "keyword.control.flow");
expectScope("        case Ok(v):", "Ok", "support.function");
expectScope("    y = parse_digit(b)?", "?", "keyword.operator.try");
expectScope("    f = 1.5e3", "1.5e3", "constant.numeric.float");
expectScope("    n: int | None = None", "|", "keyword.operator.union");

// The Markdown injection: a wys code block opens with ``` or ~~~ (3 or more), and closes with a fence
// of the same character that is at least as long (CommonMark). The end pattern refers to the begin
// captures, so substitute them the way vscode-textmate does before matching.
const md = JSON.parse(readFileSync(join(here, "..", "syntaxes", "markdown-wys.tmLanguage.json"), "utf8"));
const fence = md.patterns[0];
function firstMatch(pattern, line) {
  const scanner = new oniguruma.OnigScanner([pattern]);
  const m = scanner.findNextMatchSync(new oniguruma.OnigString(line), 0);
  return m ? m.captureIndices.map((c) => line.slice(c.start, c.end)) : null;
}
function closes(open, close) {
  const b = firstMatch(fence.begin, open);
  if (!b) {
    return null;
  }
  const end = fence.end.replace(/\\(\d+)/g, (_, n) => (b[Number(n)] ?? "").replace(/[`~]/g, (c) => "\\" + c));
  return firstMatch(end, close) !== null;
}
for (const [open, close, want] of [
  ["```wys", "```", true],
  ["```wys", "````", true],
  ["````wys fragment", "````", true],
  ["````wys", "```", false],
  ["~~~wys", "~~~~", true],
  ["```wys", "~~~", false],
]) {
  const got = closes(open, close);
  if (got !== want) {
    console.error(`FAIL fence ${JSON.stringify(open)} closed by ${JSON.stringify(close)}: want ${want}, got ${got}`);
    failures++;
  }
}
if (firstMatch(fence.begin, "```python") !== null) {
  console.error("FAIL a python block must not open a wys block");
  failures++;
}

// Every example tokenizes, and no line ends inside an unterminated string.
const examples = join(root, "examples");
let files = 0;
for (const name of readdirSync(examples).filter((n) => n.endsWith(".wys"))) {
  files++;
  for (const t of tokenize(readFileSync(join(examples, name), "utf8"))) {
    if (t.scopes.some((s) => s.startsWith("invalid."))) {
      console.error(`FAIL examples/${name}: invalid token ${JSON.stringify(t.text)}`);
      failures++;
    }
  }
}
if (files === 0) {
  console.error("FAIL no examples found");
  failures++;
}
if (failures > 0) {
  process.exit(1);
}
console.log(`grammar: ok (${files} examples)`);
