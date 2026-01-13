# Compute Efficiency Anti-Pattern Catalog

Reference for identifying compute inefficiencies in AI research codebases.

## How to Use This Catalog

Each pattern includes:
- **Problem**: What the inefficient code looks like
- **Impact**: Why this wastes compute/memory
- **Detection**: Code patterns to search for
- **Fix**: How to correct it
- **Example**: Before/after code
- **Severity**: Critical / High / Medium / Low
- **Confidence indicators**: When to be more or less certain

---

## Category 1: Model Loading & State Management

### 1.1 Reloading Model to Reset After Weight Modifications

**Severity:** Critical
**Confidence:** High (90%)

**Problem:** Loading a model from disk inside a loop to reset weights after modifications.

**Impact:** Model loading takes 30+ seconds for large models. With many iterations, this adds minutes to hours of pure loading time.

**Detection patterns:**
- `from_pretrained()` inside `for` loops
- Model loading after weight modifications like `weight += vector` or `weight.data.copy_()`

**Fix:** Clone weights before modification, restore after.

**Example:**
```python
# BAD - 30+ seconds per iteration
for steering_vector in steering_vectors:
    model = AutoModelForCausalLM.from_pretrained(model_path)
    model.layers[15].mlp.weight += steering_vector
    results.append(evaluate(model))

# GOOD - clone and restore
original_weights = model.layers[15].mlp.weight.clone()
for steering_vector in steering_vectors:
    model.layers[15].mlp.weight.copy_(original_weights + steering_vector)
    results.append(evaluate(model))
    model.layers[15].mlp.weight.copy_(original_weights)

# BEST - use hooks for temporary interventions
def steering_hook(module, input, output):
    return output + steering_vector

for steering_vector in steering_vectors:
    handle = model.layers[15].mlp.register_forward_hook(steering_hook)
    results.append(evaluate(model))
    handle.remove()
```

---

### 1.2 Loading Full Model When Only Embeddings Needed

**Severity:** High
**Confidence:** 75%

**Problem:** Loading entire causal LM when only embedding layer is used.

**Impact:** Loads 10x more parameters than needed, wastes VRAM.

**Detection patterns:**
- Model loaded, then only `.embed_tokens`, `.wte`, or `.get_input_embeddings()` accessed
- Full CausalLM loaded but only early layers used

**Fix:** Load only the embedding layer or use a lighter model.

**Example:**
```python
# BAD
model = AutoModelForCausalLM.from_pretrained("llama-7b")
embeddings = model.get_input_embeddings()(input_ids)

# GOOD
from transformers import AutoModel
model = AutoModel.from_pretrained("llama-7b")
embeddings = model.embed_tokens(input_ids)
```

---

### 1.3 Loading Same Model Multiple Times

**Severity:** High
**Confidence:** 70%

**Problem:** Multiple `from_pretrained()` calls with identical model paths in same execution.

**Impact:** Redundant memory allocation and loading time.

**Detection patterns:**
- Same model path in multiple `from_pretrained()` calls
- Model loaded in different functions without passing as parameter

**Fix:** Load once, pass model as parameter or use global/singleton pattern.

---

### 1.4 Deep Copying Models Instead of Cloning Tensors

**Severity:** Medium
**Confidence:** 80%

**Problem:** Using `copy.deepcopy(model)` when only specific tensors need cloning.

**Impact:** Copies entire model including all buffers and parameters unnecessarily.

**Detection patterns:**
- `copy.deepcopy(model)`
- `import copy` with model copying

**Fix:** Clone only the specific tensors you need to preserve.

**Example:**
```python
# BAD
model_copy = copy.deepcopy(model)

# GOOD - if you only need to preserve one layer's weights
original_weights = model.layers[15].mlp.weight.clone()
```

---

### 1.5 Multiple Large Models Loaded Simultaneously

**Severity:** Medium
**Confidence:** 75%

**Problem:** Loading multiple large models into memory at once for comparison studies.

**Impact:** May exceed VRAM, forcing swap or OOM.

**Detection patterns:**
- Multiple `from_pretrained()` calls for different models without `del` between them
- Lists or dicts containing multiple loaded models

**Fix:** Load sequentially, delete before loading next.

**Example:**
```python
# BAD
model_a = AutoModel.from_pretrained("model-a")
model_b = AutoModel.from_pretrained("model-b")
compare(model_a, model_b)

# GOOD
model_a = AutoModel.from_pretrained("model-a")
results_a = evaluate(model_a)
del model_a
torch.cuda.empty_cache()

model_b = AutoModel.from_pretrained("model-b")
results_b = evaluate(model_b)
compare(results_a, results_b)
```

---

## Category 2: Forward Pass Efficiency

### 2.1 Missing torch.no_grad() or inference_mode() During Evaluation

**Severity:** High
**Confidence:** 90%

**Problem:** Running forward passes without disabling gradient computation.

**Impact:** Stores activations for backprop (2-4x memory), slower execution.

**Detection patterns:**
- `model(inputs)` or `model.generate()` outside `torch.no_grad()` context
- `model.eval()` without accompanying `torch.no_grad()`
- Inference code without `@torch.inference_mode()` decorator

**Fix:** Wrap inference in `torch.no_grad()` or use `@torch.inference_mode()`.

**Example:**
```python
# BAD - builds unnecessary computation graph
outputs = model(inputs)
activations = outputs.hidden_states

# GOOD
with torch.inference_mode():
    outputs = model(inputs)
    activations = outputs.hidden_states

# ALSO GOOD
@torch.inference_mode()
def get_activations(model, inputs):
    return model(inputs).hidden_states
```

---

### 2.2 Repeated Forward Passes on Identical Inputs

**Severity:** High
**Confidence:** 80%

**Problem:** Same input passed to model multiple times across different code paths.

**Impact:** Wastes compute time, especially for large models.

**Detection patterns:**
- Same variable passed to `model()` multiple times
- Forward pass inside inner loop when could be outside

**Fix:** Cache outputs, restructure to compute once.

**Example:**
```python
# BAD - forward pass repeated for each layer analysis
for layer_idx in layers_to_analyze:
    outputs = model(input_ids, output_hidden_states=True)
    analyze(outputs.hidden_states[layer_idx])

# GOOD - single forward pass
outputs = model(input_ids, output_hidden_states=True)
for layer_idx in layers_to_analyze:
    analyze(outputs.hidden_states[layer_idx])
```

---

### 2.3 Multiple Forward Passes to Collect Different Layer Activations

**Severity:** High
**Confidence:** 85%

**Problem:** Running separate forward passes to get activations from different layers.

**Impact:** N forward passes instead of 1.

**Detection patterns:**
- Separate function calls to get activations from different layers
- Loop over layers with forward pass inside

**Fix:** Use `output_hidden_states=True` or register hooks for all layers at once.

**Example:**
```python
# BAD - N forward passes for N layers
layer_5_acts = get_activations(model, inputs, layer=5)
layer_10_acts = get_activations(model, inputs, layer=10)
layer_15_acts = get_activations(model, inputs, layer=15)

# GOOD - single forward pass with hooks
acts = collect_activations(model, inputs, layers=[5, 10, 15])

# OR use output_hidden_states
outputs = model(inputs, output_hidden_states=True)
layer_5_acts = outputs.hidden_states[5]
layer_10_acts = outputs.hidden_states[10]
layer_15_acts = outputs.hidden_states[15]
```

---

### 2.4 Processing Examples One-at-a-Time Instead of Batching

**Severity:** Critical
**Confidence:** 90%

**Problem:** Processing inputs individually instead of in batches.

**Impact:** Massive GPU underutilization, 10-100x slower.

**Detection patterns:**
- `for example in dataset: model(example)`
- `batch_size=1` hardcoded
- Sequential processing with model call inside loop

**Fix:** Use DataLoader with appropriate batch size.

**Example:**
```python
# BAD - GPU mostly idle
results = []
for example in dataset:
    results.append(model(example))

# GOOD
dataloader = DataLoader(dataset, batch_size=32)
results = [model(batch) for batch in dataloader]
```

---

### 2.5 Inefficient Padding Strategy

**Severity:** Medium
**Confidence:** 70%

**Problem:** Padding all sequences to model max length rather than batch max length.

**Impact:** Wastes compute on padding tokens.

**Detection patterns:**
- `padding="max_length"` with large `max_length`
- Fixed padding length regardless of actual sequence lengths

**Fix:** Use `padding=True` (pads to longest in batch) or dynamic batching.

**Example:**
```python
# BAD - pads everything to 2048
tokens = tokenizer(texts, padding="max_length", max_length=2048)

# GOOD - pads to longest in batch
tokens = tokenizer(texts, padding=True)
```

---

## Category 3: Hooks & Activation Handling

### 3.1 Hooks Registered But Never Removed

**Severity:** High
**Confidence:** 80%

**Problem:** Registering hooks without removing them after use.

**Impact:** Memory leak, hooks accumulate across iterations.

**Detection patterns:**
- `register_forward_hook()` without corresponding `handle.remove()`
- Hooks registered in loop without cleanup
- Hook handles not stored for later removal

**Fix:** Store handles and remove after use, or use context manager pattern.

**Example:**
```python
# BAD - hooks never cleaned up
for layer in layers_to_probe:
    model.layers[layer].register_forward_hook(save_activation)
    # hooks persist forever!

# GOOD - explicit removal
handles = []
for layer in layers_to_probe:
    handles.append(model.layers[layer].register_forward_hook(save_activation))
# ... do work ...
for handle in handles:
    handle.remove()

# BETTER - context manager pattern
@contextmanager
def temporary_hooks(model, layers, hook_fn):
    handles = [model.layers[l].register_forward_hook(hook_fn) for l in layers]
    try:
        yield
    finally:
        for h in handles:
            h.remove()
```

---

### 3.2 Not Using Hooks When Interventions Are Temporary

**Severity:** Medium
**Confidence:** 85%

**Problem:** Direct weight modification for activation steering when hooks would work.

**Impact:** Risk of forgetting to reset weights, more complex state management.

**Detection patterns:**
- `weight.data += vector` followed by `weight.data -= vector`
- Weight modification in loop with restoration

**Fix:** Use forward hooks for temporary interventions.

**Example:**
```python
# BAD - modifies weights directly
model.layers[15].mlp.weight.data += steering_vector
output = model(inputs)
model.layers[15].mlp.weight.data -= steering_vector

# GOOD - hook leaves weights untouched
def add_steering(module, input, output):
    return output + steering_vector

handle = model.layers[15].register_forward_hook(add_steering)
output = model(inputs)
handle.remove()
```

---

### 3.3 Storing Full Activation Tensors When Only Statistics Needed

**Severity:** Medium
**Confidence:** 60%

**Problem:** Keeping complete activation tensors when only aggregates are used downstream.

**Impact:** Memory scales with sequence_length * hidden_dim * layers.

**Detection patterns:**
- Appending full tensors to lists in hooks
- Storing `hidden_states` without reduction
- Later code only uses `.mean()`, `.std()`, `.norm()` on stored activations

**Fix:** Compute statistics in hook, store only what's needed.

**Example:**
```python
# BAD - stores everything
all_activations = []
def hook(m, i, o):
    all_activations.append(o.detach().clone())

# GOOD - stores only statistics
activation_stats = []
def hook(m, i, o):
    activation_stats.append({
        'mean': o.mean(dim=-1).cpu(),
        'std': o.std(dim=-1).cpu(),
    })
```

---

### 3.4 Activations Not Detached Before Storage

**Severity:** High
**Confidence:** 85%

**Problem:** Storing activations without `.detach()`.

**Impact:** Keeps entire computation graph in memory.

**Detection patterns:**
- `activations.append(output)` without `.detach()`
- Storing hook outputs directly

**Fix:** Always `.detach()` tensors stored for later analysis.

**Example:**
```python
# BAD
stored = []
def hook(m, i, o):
    stored.append(o)  # keeps graph!

# GOOD
stored = []
def hook(m, i, o):
    stored.append(o.detach())
```

---

### 3.5 Activations Not Moved to CPU Before Saving

**Severity:** Medium
**Confidence:** 85%

**Problem:** Saving GPU tensors directly to disk.

**Impact:** GPU memory stays allocated until save completes.

**Detection patterns:**
- `torch.save(activations, path)` where activations are on GPU
- No `.cpu()` before saving

**Fix:** Move to CPU before saving.

**Example:**
```python
# BAD - GPU memory stays allocated
torch.save(activations, "acts.pt")

# GOOD
torch.save(activations.cpu(), "acts.pt")
# Or move earlier
activations = activations.cpu()
torch.save(activations, "acts.pt")
```

---

## Category 4: Memory & Precision

### 4.1 FP32 When Half Precision Would Suffice

**Severity:** High
**Confidence:** 70%

**Problem:** Using full precision when half precision is sufficient.

**Impact:** 2x memory usage, slower compute.

**Detection patterns:**
- Model loaded without `torch_dtype` specification
- No `.half()` or `.bfloat16()` calls
- Explicit `dtype=torch.float32`

**Fix:** Use `torch_dtype=torch.bfloat16` for modern GPUs.

**Example:**
```python
# BAD - default FP32, uses 28GB for 7B model
model = AutoModelForCausalLM.from_pretrained(path)

# GOOD - half memory footprint
model = AutoModelForCausalLM.from_pretrained(
    path,
    torch_dtype=torch.bfloat16
)
```

**Note:** FP32 may be intentionally kept for numerical stability in some algorithms. Lower confidence if code involves sensitive numerical operations.

---

### 4.2 Accumulating Loss Tensor Without .item() for Logging

**Severity:** Medium
**Confidence:** 85%

**Problem:** Accumulating loss tensor instead of scalar value.

**Impact:** Keeps computation graph alive for a scalar that's only being logged.

**Detection patterns:**
- `total_loss += loss` without `.item()`
- Loss accumulated in training loop without detaching

**Fix:** Use `.item()` when accumulating for logging.

**Example:**
```python
# BAD - graph accumulates
total_loss = 0
for batch in loader:
    loss = model(**batch).loss
    total_loss += loss  # keeps graph!

# GOOD
total_loss = 0
for batch in loader:
    loss = model(**batch).loss
    total_loss += loss.item()  # just the number
```

---

### 4.3 Not Using Gradient Checkpointing for Long Sequences

**Severity:** Medium
**Confidence:** 60%

**Problem:** Training on long sequences without gradient checkpointing.

**Impact:** Memory scales linearly with sequence length for activations.

**Detection patterns:**
- Long `max_length` (>1024) in training code
- No `model.gradient_checkpointing_enable()` call

**Fix:** Enable gradient checkpointing for memory-constrained training.

**Example:**
```python
# Enable gradient checkpointing
model.gradient_checkpointing_enable()
```

---

### 4.4 Missing del + Cache Clear in Long-Running Loops

**Severity:** Low
**Confidence:** 50%

**Problem:** Not explicitly freeing memory between large operations.

**Impact:** Memory fragmentation can lead to OOM even with available memory.

**Detection patterns:**
- Long loops with large tensor operations
- No `del` or `torch.cuda.empty_cache()` calls

**Fix:** Explicitly delete and clear cache between major operations.

**Example:**
```python
for experiment in experiments:
    activations = collect_activations(model, data)
    results.append(analyze(activations))
    del activations
    torch.cuda.empty_cache()
```

---

## Category 5: Data Loading & Preprocessing

### 5.1 Tokenizing Same Dataset Multiple Times

**Severity:** High
**Confidence:** 85%

**Problem:** Re-tokenizing dataset on every run without caching.

**Impact:** CPU bottleneck, wasted time on every experiment.

**Detection patterns:**
- `tokenizer()` calls without cache checks
- No tokenized dataset saved to disk
- Tokenization in `__getitem__`

**Fix:** Pre-tokenize and cache to disk.

**Example:**
```python
# BAD - re-tokenizes every run
tokens = tokenizer(texts)

# GOOD - cache to disk
cache_path = "tokenized_dataset.pt"
if os.path.exists(cache_path):
    tokens = torch.load(cache_path)
else:
    tokens = tokenizer(texts)
    torch.save(tokens, cache_path)

# ALSO GOOD - HuggingFace datasets with caching
dataset = dataset.map(
    lambda x: tokenizer(x['text']),
    batched=True,
    num_proc=4
)  # automatically cached
```

---

### 5.2 Not Caching Activations for Reuse Across Experiments

**Severity:** High
**Confidence:** 85%

**Problem:** Recomputing activations that are used in multiple analyses.

**Impact:** Multiplies compute cost by number of experiments.

**Detection patterns:**
- Same model/dataset activation collection in multiple scripts
- No activation cache directory
- Multiple probing experiments without shared activations

**Fix:** Compute activations once, save to disk.

**Example:**
```python
# BAD - recomputes for each experiment
for probe_type in probe_types:
    activations = get_activations(model, dataset)
    train_probe(probe_type, activations)

# GOOD - compute once, reuse
cache_path = f"activations_{model_name}_{dataset_name}.pt"
if os.path.exists(cache_path):
    activations = torch.load(cache_path)
else:
    activations = get_activations(model, dataset)
    torch.save(activations.cpu(), cache_path)

for probe_type in probe_types:
    train_probe(probe_type, activations)
```

---

### 5.3 Repeated File I/O Inside Training Loops

**Severity:** Medium
**Confidence:** 80%

**Problem:** Reading configs, prompts, or data files on every iteration.

**Impact:** I/O bottleneck, especially on networked storage.

**Detection patterns:**
- File `open()` inside `for` loop
- JSON/YAML loading on every iteration
- Reading prompt files repeatedly

**Fix:** Load files once before loop.

---

### 5.4 Loading Full Dataset Into Memory When Streaming Viable

**Severity:** Medium
**Confidence:** 55%

**Problem:** Loading entire large dataset when single-pass access is sufficient.

**Impact:** RAM exhaustion, slow startup.

**Detection patterns:**
- `load_dataset()` without `streaming=True` for large datasets
- Reading all files into list before processing

**Fix:** Use streaming for large single-pass datasets.

**Example:**
```python
# BAD - loads all into RAM
dataset = load_dataset("huge_dataset")

# GOOD - streams from disk
dataset = load_dataset("huge_dataset", streaming=True)
```

---

## Category 6: Batching & Parallelism

### 6.1 Batch Size Not Parameterized

**Severity:** Medium
**Confidence:** 80%

**Problem:** Hardcoded batch size that can't adapt to available VRAM.

**Impact:** Either underutilizes GPU or causes OOM on smaller hardware.

**Detection patterns:**
- Magic number batch sizes (32, 64, 128) without configuration
- No batch size argument or environment variable

**Fix:** Make batch size configurable.

**Example:**
```python
# BAD
batch_size = 64  # will OOM on smaller GPUs

# GOOD
batch_size = int(os.environ.get("BATCH_SIZE", 32))
# OR
parser.add_argument("--batch_size", type=int, default=32)
```

---

### 6.2 Not Using Gradient Accumulation for Large Effective Batches

**Severity:** Medium
**Confidence:** 65%

**Problem:** Trying to fit large batch sizes when gradient accumulation would work.

**Impact:** OOM errors, limiting effective batch size.

**Detection patterns:**
- Large batch size with OOM comments
- No accumulation step logic

**Fix:** Use gradient accumulation for larger effective batches.

**Example:**
```python
accumulation_steps = 4
optimizer.zero_grad()
for i, batch in enumerate(dataloader):
    loss = model(**batch).loss / accumulation_steps
    loss.backward()
    if (i + 1) % accumulation_steps == 0:
        optimizer.step()
        optimizer.zero_grad()
```

---

### 6.3 Sequences Not Sorted by Length Before Batching

**Severity:** Low
**Confidence:** 65%

**Problem:** Random order batching leads to excessive padding.

**Impact:** Wasted compute on padding tokens.

**Detection patterns:**
- Variable-length sequences without length sorting
- High padding ratio visible in tokenized data

**Fix:** Sort by length or use bucket batching.

---

## Category 7: Checkpointing & Reproducibility

### 7.1 No Checkpointing in Long-Running Jobs

**Severity:** High
**Confidence:** 75%

**Problem:** Training or processing without saving intermediate state.

**Impact:** Total loss of progress on crash.

**Detection patterns:**
- Long training loops without `save_pretrained()` or `torch.save()`
- No checkpoint directory configuration
- Hours of processing without saves

**Fix:** Checkpoint regularly.

**Example:**
```python
# BAD - crash at epoch 99 = start over
for epoch in range(100):
    train(model)

# GOOD
for epoch in range(100):
    train(model)
    if epoch % 10 == 0:
        model.save_pretrained(f"checkpoint-{epoch}")
```

---

### 7.2 No Deterministic Seeding

**Severity:** Medium
**Confidence:** 70%

**Problem:** Missing random seed setting for reproducibility.

**Impact:** Can't reproduce results, debugging harder.

**Detection patterns:**
- No `torch.manual_seed()`, `random.seed()`, `np.random.seed()` calls
- No seed configuration

**Fix:** Set seeds at start of script.

**Example:**
```python
def set_seed(seed):
    random.seed(seed)
    np.random.seed(seed)
    torch.manual_seed(seed)
    torch.cuda.manual_seed_all(seed)

set_seed(42)
```

---

### 7.3 Saving Full Optimizer State Unnecessarily

**Severity:** Low
**Confidence:** 50%

**Problem:** Checkpoints include optimizer state when resumption isn't needed.

**Impact:** Checkpoints 2-3x larger than necessary.

**Detection patterns:**
- `torch.save({'model': ..., 'optimizer': ...})` when optimizer state unused
- Large checkpoint files

**Fix:** Only save optimizer state if you'll resume training.

---

## VRAM Estimation Reference

### Common Model Sizes

| Model | Parameters | FP32 VRAM | BF16 VRAM | INT8 VRAM |
|-------|-----------|-----------|-----------|-----------|
| GPT-2 | 124M | ~500MB | ~250MB | ~125MB |
| GPT-2 Medium | 355M | ~1.4GB | ~700MB | ~350MB |
| GPT-2 Large | 774M | ~3GB | ~1.5GB | ~750MB |
| GPT-2 XL | 1.5B | ~6GB | ~3GB | ~1.5GB |
| Llama-2-7B | 7B | ~28GB | ~14GB | ~7GB |
| Llama-2-13B | 13B | ~52GB | ~26GB | ~13GB |
| Llama-2-70B | 70B | ~280GB | ~140GB | ~70GB |
| Mistral-7B | 7B | ~28GB | ~14GB | ~7GB |
| Pythia-70M | 70M | ~280MB | ~140MB | ~70MB |
| Pythia-410M | 410M | ~1.6GB | ~800MB | ~400MB |
| Pythia-1B | 1B | ~4GB | ~2GB | ~1GB |
| Pythia-6.9B | 6.9B | ~28GB | ~14GB | ~7GB |
| Pythia-12B | 12B | ~48GB | ~24GB | ~12GB |
| Gemma-2B | 2B | ~8GB | ~4GB | ~2GB |
| Gemma-7B | 7B | ~28GB | ~14GB | ~7GB |

### Estimation Rules

1. **Base model memory**: ~2 bytes per parameter (BF16) or ~4 bytes (FP32)
2. **Inference overhead**: Add ~20-50% for KV cache and activations
3. **Training overhead**: Add ~3-4x for gradients and optimizer states (Adam)
4. **Activation caching**: Scales with batch_size * sequence_length * hidden_dim * num_layers_cached

### Detection Patterns for Model Identification

Look for these patterns to identify models:
- `from_pretrained("model-name")` - extract model name
- Common prefixes: `llama`, `gpt`, `mistral`, `phi`, `gemma`, `pythia`
- Size indicators: `7b`, `13b`, `70b`, `7B`, `13B`, `70B`
- HuggingFace paths: `meta-llama/Llama-2-7b-hf`, `EleutherAI/pythia-6.9b`
