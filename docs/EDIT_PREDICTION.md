# Edit Prediction (self-hosted)

Zedium keeps Zed's inline edit-prediction UI but removes the Zed-hosted prediction
service entirely. Predictions work **only** against a model you run or pay for yourself:

- **Ollama** — a local model served by [Ollama](https://ollama.com).
- **OpenAI-compatible** — any private/self-hosted server exposing a text-completion endpoint
  (vLLM, llama.cpp `server`, TGI, LocalAI, etc.).
- **Mercury** (Inception) — BYO API key.
- **Codestral** (Mistral) — BYO API key.

There is no default and no sign-in. With nothing configured, edit prediction is simply off and
makes no network calls.

## The Zeta open-weights model

Zed's edit-prediction model ("Zeta") is published as open weights. The client-side prompt
construction (`zeta_prompt`) is retained, so you can run the same model locally and get the same
behaviour, without any Zed account.

Download the weights from the model card (e.g. `huggingface.co/zed-industries/zeta`) and serve
them through one of the backends below. Confirm the weight license on the model card; Zedium
neither hosts nor redistributes weights.

## Ollama

```sh
# Pull / create the model in Ollama, then point Zedium at it.
ollama serve   # default: http://localhost:11434
```

`settings.json`:

```jsonc
{
  "edit_predictions": {
    "provider": "ollama",
    "ollama": {
      "api_url": "http://localhost:11434",
      "model": "zeta"
    }
  }
}
```

Zedium sends a raw completion (`"raw": true`) so Ollama does not apply its own chat template —
the `zeta_prompt` format reaches the model unmodified.

## OpenAI-compatible private server

Serve the model behind any OpenAI-style text-completion endpoint, e.g. vLLM:

```sh
python -m vllm.entrypoints.openai.api_server --model /path/to/zeta --port 8000
```

`settings.json`:

```jsonc
{
  "edit_predictions": {
    "provider": "openai_compatible",
    "open_ai_compatible_api": {
      "api_url": "http://127.0.0.1:8000/v1/completions",
      "model": "zeta"
    }
  }
}
```

If your server requires a key, set it in the environment:

```sh
export ZEDIUM_OPEN_AI_COMPATIBLE_EDIT_PREDICTION_API_KEY="<token>"
```

## Mercury / Codestral (BYO key)

These are third-party hosted providers; you supply your own key. Select `mercury` or `codestral`
as the provider and add the key through the edit-prediction settings UI (status-bar menu → provider).

## Verifying it works

1. Status bar shows the edit-prediction indicator when a provider is configured.
2. Type in a buffer; predictions appear as inline ghosts (accept with `tab`).
3. If nothing appears, check the endpoint is reachable and the model name matches what the server
   exposes. Zedium makes no fallback call to any Zed endpoint — a misconfiguration means no
   prediction, never a silent cloud request.

## What was removed

The Zed-hosted predict path (`/predict_edits/*`), A/B experiments, usage metering, data
collection, and account/billing gating were deleted (patches 0023–0025). Reintroducing any of
them fails CI (see `tools/forbidden-strings.txt`).
