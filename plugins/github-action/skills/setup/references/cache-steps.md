# Dependency Setup Steps for GitHub Actions

YAML snippets to insert into Claude Code GitHub Action workflows between the checkout step and the main job steps. Each snippet handles toolchain setup, dependency caching, and dependency installation. Indented for direct insertion into the workflow YAML (6 spaces, matching the `steps:` block).

## Python + uv

Detect: `uv.lock` or `pyproject.toml` with no other lockfile

```yaml
      - name: Set up uv
        uses: astral-sh/setup-uv@v7
        with:
          enable-cache: true

      - name: Install dependencies
        run: uv sync
```

## Python + pip

Detect: `requirements.txt` with no `uv.lock`

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

Detect: `Cargo.toml`

```yaml
      - name: Cache Rust dependencies
        uses: Swatinem/rust-cache@v2
```

## Node.js + npm

Detect: `package.json` + `package-lock.json`

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

Detect: `package.json` + `bun.lock` or `bun.lockb`

```yaml
      - name: Set up Bun
        uses: oven-sh/setup-bun@v2

      - name: Cache bun dependencies
        uses: actions/cache@v4
        with:
          path: ~/.bun/install/cache
          key: bun-${{ runner.os }}-${{ hashFiles('**/bun.lock') }}
          restore-keys: |
            bun-${{ runner.os }}-

      - name: Install dependencies
        run: bun install --frozen-lockfile
```

## No match / multiple ecosystems

If no ecosystem is detected or multiple are present, leave the `# CACHE_STEP` comment as-is and tell the user to add a cache step manually.
