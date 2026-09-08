# Dependency Setup Steps for GitHub Actions

Per-ecosystem snippets that replace the `# CACHE_STEP` line in both workflows. Each one installs the toolchain, caches dependencies, and installs them; the indentation (6 spaces) already matches the `steps:` block.

## Python + uv

```yaml
      - name: Set up uv
        uses: astral-sh/setup-uv@v7
        with:
          enable-cache: true

      - name: Install dependencies
        run: uv sync
```

## Python + pip

```yaml
      - name: Set up Python
        uses: actions/setup-python@v6
        with:
          python-version: '3.13'
          cache: 'pip'

      - name: Install dependencies
        run: pip install -r requirements.txt
```

## Rust

```yaml
      - name: Cache Rust dependencies
        uses: Swatinem/rust-cache@v2
```

## Node.js + npm

```yaml
      - name: Set up Node.js
        uses: actions/setup-node@v6
        with:
          node-version: '22'
          cache: 'npm'

      - name: Install dependencies
        run: npm ci
```

## Node.js + bun

```yaml
      - name: Set up Bun
        uses: oven-sh/setup-bun@v2

      - name: Cache bun dependencies
        uses: actions/cache@v4
        with:
          path: ~/.bun/install/cache
          key: bun-${{ runner.os }}-${{ hashFiles('**/bun.lock', '**/bun.lockb') }}
          restore-keys: |
            bun-${{ runner.os }}-

      - name: Install dependencies
        run: bun install --frozen-lockfile
```

