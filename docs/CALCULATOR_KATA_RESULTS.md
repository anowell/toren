# Calculator Kata Results

End-to-end test proving Toren works: Claude built a complete CLI calculator with zero manual coding.

## Summary

- **31 tests, all passing**
- **~60 seconds** to build
- **~350 lines** of code generated

## What Claude Built

- `calculator.js` - Expression parser with order of operations
- `calculator.test.js` - 31 test cases
- `package.json` - Node.js config

## Test Output

```
$ npm test
✔ addition: 3 + 2 = 5
✔ order of operations: 3 + 2 * 4 = 11
✔ decimals: 3.5 + 2.5 = 6
✔ error handling: division by zero throws
... (31 tests total)

ℹ tests 31
ℹ pass 31
```

## Run It Yourself

```bash
just test-calculator
```

## What This Proves

1. Daemon API works end-to-end
2. Claude can build complete applications
3. Tool calling, auth, command streaming all functional
4. VCS integration works
