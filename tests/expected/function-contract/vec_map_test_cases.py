#!/usr/bin/env python3
from subprocess import run, TimeoutExpired
from pathlib import Path
import os
import argparse
from collections import namedtuple, abc

DEFAULT_KEY_TYPE = "u8"
DEFAULT_VALUE_TYPE = "u8"

def mvm(name: str, key_t: str = DEFAULT_KEY_TYPE, val_t: str = DEFAULT_VALUE_TYPE, method_types: abc.Iterable[str] = None) -> str:
    '''
    Monomorphized VecMap. This function qualifies the unqualified method `name`
    with `VecMap` and the key type `key_t` and value type `val_t`.
    '''
    extra = "" if method_types is None else "::<" + ", ".join(method_types) + ">"
    return f"VecMap::<{key_t}, {val_t}>::{name}{extra}"

def ch(name: str) -> str:
    return "contract_harnesses::" + name

TestCase = namedtuple("TestCase", ["function", "harness", "skip"], defaults=[None])

DEFAULT_TAKES_TOO_LONG="does not terminate in a very long time, even with `unwind`"

TEST_CASES = [
    TestCase(mvm("new"), ch("new_1")),
    TestCase(mvm("new", key_t = "u64", val_t = "std::option::Option<i32>"), ch("new_2")),
    TestCase(mvm("with_capacity"), ch("with_capacity")),
    TestCase(mvm("clear"), ch("clear"), skip="it times out"),
    TestCase(mvm("get", method_types=[DEFAULT_KEY_TYPE]), ch("get"), skip=DEFAULT_TAKES_TOO_LONG),
    TestCase(mvm("get_mut", method_types=[DEFAULT_KEY_TYPE]), ch("get_mut"), skip=DEFAULT_TAKES_TOO_LONG),
    TestCase(mvm("insert"), ch("insert"), skip="`old` is not yet implemented"),
    TestCase(mvm("get_key_value", method_types=[DEFAULT_KEY_TYPE]), ch("get_key_value"), skip=DEFAULT_TAKES_TOO_LONG),
    TestCase(mvm("remove", method_types=[DEFAULT_KEY_TYPE]), ch("remove"), skip=DEFAULT_TAKES_TOO_LONG),
    TestCase(mvm("remove_entry", method_types=[DEFAULT_KEY_TYPE]), ch("remove_entry"), skip=DEFAULT_TAKES_TOO_LONG)
]

TEST_FILE = "fixme_vec_map_example.rs"

def run_tests(test_cases: abc.Iterable[TestCase], args):
    timeout = args.timeout
    for tc in test_cases:
        if tc.skip is not None and args.index is None and not args.noskip:
            print(f"Skipping function contract for {tc.function} because {tc.skip}")
            continue
        print(f"Checking function contract for {tc.function} on {tc.harness} ... ", end='', flush=True)
        try:
            ret = run(("kani", "--check-contract", f"{tc.function}/{tc.harness}", TEST_FILE), capture_output=True, timeout=timeout, text=True)
            if check_and_report_cmd_result(ret, args.fail_fast):
                print("finished successfully")
            elif args.fail_fast:
                return
        except TimeoutExpired:
            print(f"timed out after {timeout}s")
            if args.fail_fast:
                return

def check_and_report_cmd_result(result, print_output=True):
    if result.returncode != 0:
        print(f"exited with code {result.returncode}")
        if print_output:
            print("------------ stdout ------------")
            print(result.stdout)
            print("------------ stderr ------------")
            print(result.stderr)
        return False
    return True

def list_available():
    col_0_width = 1
    col_1_width = 20
    col_2_width = 20
    for i, tc in enumerate(TEST_CASES):
        col_0_width = max(col_0_width, len(str(i)))
        col_1_width = max(col_1_width, len(tc.function))
        col_2_width = max(col_2_width, len(tc.harness))
    print(f" {'#':^{ col_0_width}} | {'Function':^{ col_1_width}} | {'Harness':^{ col_2_width}}")
    print(f"-{'' :-^{col_0_width}}-+-{''        :-^{col_1_width}}-+-{''       :-^{col_2_width}}")
    for i, tc in enumerate(TEST_CASES):
        print(f" {i:>{ col_0_width}} | {tc.function:<{ col_1_width}} | {tc.harness:<{ col_2_width}}")

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--fail-fast", action="store_true", help="Abort after the first failed verification or timeout")
    parser.add_argument("--index", type=int, help="Run test with this number")
    parser.add_argument("--timeout", type=float, default=30.0, help="Abandon verification after this many seconds")
    parser.add_argument("--filter", help="Only run contracts which match this pattern")
    parser.add_argument("--list", action='store_true')
    parser.add_argument("--noskip", action='store_true')
    args = parser.parse_args()

    if args.list:
        list_available()
        return

    print("Building kani ... ", end='', flush=True)

    mypath = Path(__file__)
    basedir = mypath.parent.parent.parent.parent
    scriptdir = basedir / "scripts"

    build_res = run(["cargo", "build-dev"], cwd=basedir, capture_output=True)
    if not check_and_report_cmd_result(build_res):
        return
    print("done")

    oldpath = os.environ["PATH"]

    if not str(scriptdir) in oldpath:
        os.environ["PATH"] = f"{scriptdir}:{oldpath}"

    test_cases = TEST_CASES 
    if args.index is not None:
        assert args.filter is None, "Cannot specify --filter and --index together"
        test_cases = [TEST_CASES[args.index]]
    elif args.filter is not None:
        test_cases = [
            tc 
            for tc in TEST_CASES
            if args.filter in tc.function
        ]

    run_tests(test_cases, args)

if __name__ == '__main__':
    main()