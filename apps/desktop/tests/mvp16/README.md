# MVP-16 independent component checks

This test surface imports the owned `ui/**` and `features/home/**` modules. It does not register a Tauri command, production route, or backend test double.

## Contract checks

From the repository root, after installing the existing Desktop dependencies:

```bash
node --experimental-strip-types --test apps/desktop/tests/mvp16/contracts/*.test.ts
```

The tests cover independent query refreshes, stale response rejection, Operation sequence monotonicity, safe save presentation, subscription check A versus save B, `user_configured` visibility, unknown price, and local presentation preferences.

## Browser surface

```bash
npm --prefix apps/desktop exec vite -- tests/mvp16/browser --host 127.0.0.1 --port 4176
```

Example deterministic URLs:

- `http://127.0.0.1:4176/?scenario=fresh&lang=zh&theme=dark&scale=1`
- `http://127.0.0.1:4176/?scenario=partial&lang=en&theme=light&scale=1.5`
- `http://127.0.0.1:4176/?scenario=drift&lang=zh&theme=dark&scale=2`

The surface records navigation intents as local JSON only. It cannot prove model saving, Agent calls, Worker delegation, or any Desktop-to-backend production path.
