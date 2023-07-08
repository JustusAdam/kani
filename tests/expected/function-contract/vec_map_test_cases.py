#!/usr/bin/env python3
from subprocess import run, TimeoutExpired
from pathlib import Path
import os
import argparse
from collections import namedtuple, abc
import time
import shlex

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
    TestCase(mvm("clear"), ch("clear")),
    TestCase(mvm("get", method_types=[DEFAULT_KEY_TYPE]), ch("get")),
    TestCase(mvm("get_mut", method_types=[DEFAULT_KEY_TYPE]), ch("get_mut")),
    TestCase(mvm("insert"), ch("insert"), skip="`old` is not implemented"),
    TestCase(mvm("get_key_value", method_types=[DEFAULT_KEY_TYPE]), ch("get_key_value")),
    TestCase(mvm("remove", method_types=[DEFAULT_KEY_TYPE]), ch("remove")),
    TestCase(mvm("remove_entry", method_types=[DEFAULT_KEY_TYPE]), ch("remove_entry"))
]

TEST_FILE = "fixme_vec_map_example.rs"

def run_tests(test_cases: abc.Iterable[TestCase], args):
    timeout = args.timeout
    for tc in test_cases:
        skip = tc.skip is not None and args.index is None and not args.noskip
        cmd = ("kani", "--default-unwind", "10", "--check-contract", f"{tc.function}/{tc.harness}", args.test_file)
        if args.echo_commands and not skip:
            print(shlex.join(cmd))
        print(f"Checking function contract for {tc.function} on {tc.harness} ... ", end='', flush=True)
        if skip:
            print(f"{TC.lightblue}skipped{TC.reset} because {tc.skip}")
            continue
        try:
            before = time.time()
            ret = run(cmd, capture_output=True, timeout=timeout, text=True)
            if not check_and_report_cmd_result(ret, before, args.fail_fast or args.verbose) and args.fail_fast:
                return
        except TimeoutExpired:
            print(f"{TC.yellow}timed out{TC.reset} after {timeout}s")
            if args.fail_fast:
                return

def check_and_report_cmd_result(result, began=None, print_output=True):
    success = True
    if result.returncode != 0:
        print(f"{TC.red}failure{TC.reset}, exited with code {result.returncode}")
        success = False
    else:
        after = time.time()
        t = f" in {round(after - began, ndigits=1)} seconds" if began is not None else ""
        print(f"{TC.green}finished successfully{TC.reset}{t}")
    if print_output:
        print("------------ stdout ------------")
        print(result.stdout)
        print("------------ stderr ------------")
        print(result.stderr)
    return success

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

class TC:
    reset = '\033[0m' 
    green = '\033[32m'
    red = '\033[31m'
    yellow = '\033[93m'
    lightblue = '\033[94m'

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--fail-fast", action="store_true", help="Abort after the first failed verification or timeout")
    parser.add_argument("--index", type=int, help="Run test with this number")
    parser.add_argument("--timeout", type=float, default=30.0, help="Abandon verification after this many seconds")
    parser.add_argument("--filter", help="Only run contracts which match this pattern")
    parser.add_argument("--list", action='store_true')
    parser.add_argument("--noskip", action='store_true')
    parser.add_argument("--echo-commands", action='store_true')
    parser.add_argument("--test-file", default=TEST_FILE)
    parser.add_argument("--verbose", action='store_true')
    args = parser.parse_args()

    if args.list:
        list_available()
        return

    print("Building kani ... ", end='', flush=True)

    mypath = Path(__file__)
    basedir = mypath.parent.parent.parent.parent
    scriptdir = basedir / "scripts"

    build_res = run(["cargo", "build-dev"], cwd=basedir, capture_output=True, text=True)
    if not check_and_report_cmd_result(build_res):
        return

    oldpath = os.environ["PATH"]

    if not str(scriptdir) in oldpath:
        os.environ["PATH"] = f"{scriptdir}:{oldpath}"

    test_cases = TEST_CASES 
    if args.index is not None:
        assert args.filter is None, f"{TC.red}Cannot specify --filter and --index together{TC.reset}"
        assert args.filter < len(TEST_CASES), f"{TC.red}index ({args.index}) must be smaller than the number of test cases ({len(TEST_CASES)}){TC.reset}"
        test_cases = [TEST_CASES[args.index]]
    elif args.filter is not None:
        test_cases = [
            tc 
            for tc in TEST_CASES
            if args.filter in tc.function
        ]
    
    if len(test_cases) == 0:
        print(f"{TC.yellow}Your filter is too strict and selected no test cases{TC.reset}, available test cases are:")
        list_available()
        return

    run_tests(test_cases, args)

if __name__ == '__main__':
    main()