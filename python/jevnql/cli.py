"""`jevnql`: ask questions about CSV / Parquet files in natural language.

    jevnql data/*.csv                       interactive shell
    jevnql data/*.csv -q "question"         one question
    jevnql data/*.csv --plan plan.json      run a JevIR plan (no LLM needed)

In the shell, prefix a question with EXPLAIN to see the plans without running
them. Commands: \\tables, \\ir (last plan as JSON), \\help, \\q.
"""

from __future__ import annotations

import argparse
import json
import sys

from jevnql.bindings import Core, CoreError
from jevnql.compiler import AnthropicLLM, CompileError, Compiler, LLMError

HELP = """\
  <question>            compile, optimize and run a question
  EXPLAIN <question>    show the logical plan, optimizer rewrites and physical plan
  \\tables               list tables and columns
  \\ir                   print the last plan as JevIR JSON
  \\q                    quit"""


class Shell:
    def __init__(self, core: Core, optimize: bool = True):
        self.core = core
        self.optimize = optimize
        self.compiler: Compiler | None = None
        self.last_plan: dict | None = None

    def _compiler(self) -> Compiler:
        if self.compiler is None:
            self.compiler = Compiler(self.core, AnthropicLLM())
        return self.compiler

    def run_plan(self, plan: dict, explain_only: bool = False) -> bool:
        self.last_plan = plan
        out = self.core.run(plan, explain_only=explain_only, optimize=self.optimize)
        if not out["ok"]:
            print(f"error: {out['error']}", file=sys.stderr)
            return False
        print(out["text"])
        return True

    def ask(self, question: str, explain_only: bool = False) -> bool:
        print("Planning...\n", flush=True)
        try:
            compiled = self._compiler().compile(question)
        except (CompileError, LLMError) as e:
            print(f"error: {e}", file=sys.stderr)
            return False
        except Exception as e:  # e.g. missing Anthropic credentials
            print(
                f"error: cannot reach the language model ({e}).\n"
                "Set ANTHROPIC_API_KEY (or run `ant auth login`), or run a JevIR plan with --plan.",
                file=sys.stderr,
            )
            return False
        for assumption in compiled.assumptions:
            print(f"  assumption: {assumption}")
        if compiled.assumptions:
            print()
        return self.run_plan(compiled.plan, explain_only)

    def tables(self) -> None:
        for table in self.core.catalog():
            cols = ", ".join(f"{c['name']}: {c['type']}" for c in table["columns"])
            print(f"  {table['name']} ({table['rows']:,} rows): {cols}")

    def loop(self) -> None:
        print(f"JevNQL — {len(self.core.files)} file(s). Type \\help for commands.\n")
        self.tables()
        while True:
            try:
                line = input("\nJevNQL > ").strip()
            except (EOFError, KeyboardInterrupt):
                print()
                return
            if not line:
                continue
            if line in ("\\q", "\\quit", "exit", "quit"):
                return
            if line == "\\help":
                print(HELP)
            elif line == "\\tables":
                self.tables()
            elif line == "\\ir":
                print(json.dumps(self.last_plan, indent=2) if self.last_plan else "(no plan yet)")
            elif line.upper().startswith("EXPLAIN "):
                self.ask(line[len("EXPLAIN ") :].strip().strip('"'), explain_only=True)
            else:
                self.ask(line)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="jevnql", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("files", nargs="+", help=".csv / .parquet files; each becomes a table named after the file")
    parser.add_argument("-q", "--question", help="answer one question and exit")
    parser.add_argument("--plan", help="run a JevIR plan document instead of a question")
    parser.add_argument("--explain", action="store_true", help="show plans without executing")
    parser.add_argument("--backend", choices=["auto", "jev", "simulated"], default="auto",
                        help="semantic backend (auto: Jev if TYPESAFE_API_KEY is set, else simulated)")
    parser.add_argument("--no-optimize", action="store_true", help="execute plans exactly as written")
    parser.add_argument("--max-semantic-rows", type=int, help="cap on rows sent to one semantic operator")
    args = parser.parse_args(argv)

    try:
        with Core(args.files, backend=args.backend, max_semantic_rows=args.max_semantic_rows) as core:
            shell = Shell(core, optimize=not args.no_optimize)
            if args.plan:
                with open(args.plan) as f:
                    return 0 if shell.run_plan(json.load(f), args.explain) else 1
            if args.question:
                return 0 if shell.ask(args.question, args.explain) else 1
            shell.loop()
            return 0
    except CoreError as e:
        print(f"error: {e}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
