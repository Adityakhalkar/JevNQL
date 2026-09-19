"""Compile one question: python -m jevnql.compiler -q "question" data.csv ..."""

import argparse
import json

from jevnql.bindings import Core
from jevnql.compiler import AnthropicLLM, Compiler


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("-q", "--question", required=True)
    parser.add_argument("files", nargs="+")
    args = parser.parse_args()

    compiled = Compiler(Core(args.files), AnthropicLLM()).compile(args.question)
    for assumption in compiled.assumptions:
        print(f"assumption: {assumption}")
    print(compiled.logical_plan)
    print(json.dumps(compiled.plan, indent=2))


if __name__ == "__main__":
    main()
