# Gemini 3 Pro Image Model Call Guide

This document explains in detail how to call the Google `gemini-3-pro-image` (Imagen 3) model inside the **Antigravity** project. This project provides a fully OpenAI-protocol-compatible wrapper for this model, and additionally supports native photographic aspect ratios, person-generation safety policies, and **Image-to-Image** generation.

## 1. Basic Information

*   **Model ID**: `gemini-3-pro-image` (alias `gemini-3-pro-image-preview` supported)
*   **Endpoint paths**:
    *   `/v1/images/generations` (Text-to-Image)
    *   `/v1/images/edits` (Image-to-Image / editing)
    *   `/v1/chat/completions` (compatibility mode)
*   **Underlying model**: Google Imagen 3 (Gemini Native)

---

## 2. Text-to-Image

Call `/v1/images/generations`, which supports the following parameters:

### 2.1 Frame and Aspect Ratio (Size / Aspect Ratio)

The `size` parameter accepts two input formats; the system automatically parses and maps them to the standard ratios Gemini supports:

1.  **Direct ratio input (recommended)**: e.g. `"16:9"`, `"4:3"`, `"1:1"`. This is the most intuitive method, and maps with 100% accuracy.
2.  **Resolution input (compatibility)**: e.g. `"1920x1080"`, `"1024x1024"`. The system automatically computes the aspect ratio (e.g. 1920/1080 ~= 1.77) and normalizes it to the closest standard ratio (16:9).

**Important note**: Gemini (Imagen 3) **does not support arbitrary custom pixel sizes**.
Regardless of whether you input `"1920x1080"` or `"16:9"` in `size`, the **actual physical resolution** of the final generated image is determined only by these two factors:
1.  **Aspect ratio** (derived by parsing `size`)
2.  **Quality tier** (determined by the `quality` parameter: `1k`/`2k`/`4k`)

*Example: given `size: "1920x1080"` (16:9) and `quality: "standard"` (1k), the actual generated image size is **1376x768** (1K resolution at 16:9), not 1920x1080.*

| Target Ratio | Typical Use Case | `size` Parameter Example (resolution) | Notes |
| :--- | :--- | :--- | :--- |
| **16:9** | widescreen, cinematic | `1920x1080`, `1280x720` | standard widescreen |
| **9:16** | phone wallpaper, Stories | `1080x1920`, `720x1280` | full-screen portrait |
| **1:1** | avatar, Instagram | `1024x1024` | default ratio |
| **4:3** | traditional photography, monitors | `1024x768`, `800x600` | |
| **3:4** | portrait photography | `768x1024`, `600x800` | |
| **21:9** | ultrawide, cinema | `2560x1080` | movie screen |
| **3:2** | **[New]** full-frame DSLR | `1500x1000` | classic photography ratio |
| **2:3** | **[New]** vertical composition photography | `1000x1500` | posters, character art |
| **5:4** | **[New]** large format | `1280x1024` | fine art photography |
| **4:5** | **[New]** vertical social media image | `1024x1280` | best display ratio on Instagram |

> **Tip**: you don't need to match pixel values exactly; as long as the aspect ratio is close to one of the ratios above (tolerance 0.05), it is auto-detected.

### 2.2 Quality and Resolution (Quality)

Controls generation fidelity via the `quality` parameter.

| Parameter Value (`quality`) | Corresponding Gemini Setting | Description |
| :--- | :--- | :--- |
| `standard` / `1k` | Image Size: `1K` | fast generation, good for quick validation (default) |
| `medium` / `2k` | Image Size: `2K` | balances quality and speed |
| `hd` / `4k` | Image Size: `4K` | **extremely high quality**, richest detail, takes somewhat longer |

#### Resolution Reference Table (Gemini 3 Pro Image)

| Aspect Ratio | 1K Resolution (Standard) | 2K Resolution (Medium) | 4K Resolution (HD) |
| :--- | :--- | :--- | :--- |
| **1:1** | 1024x1024 | 2048x2048 | 4096x4096 |
| **2:3** | 848x1264 | 1696x2528 | 3392x5056 |
| **3:2** | 1264x848 | 2528x1696 | 5056x3392 |
| **3:4** | 896x1200 | 1792x2400 | 3584x4800 |
| **4:3** | 1200x896 | 2400x1792 | 4800x3584 |
| **4:5** | 928x1152 | 1856x2304 | 3712x4608 |
| **5:4** | 1152x928 | 2304x1856 | 4608x3712 |
| **9:16** | 768x1376 | 1536x2752 | 3072x5504 |
| **16:9** | 1376x768 | 2752x1536 | 5504x3072 |
| **21:9** | 1584x672 | 3168x1344 | 6336x2688 |

### Call Example (Python)

```python
import requests

url = "http://localhost:8045/v1/images/generations"
headers = {
    "Content-Type": "application/json",
    "Authorization": "Bearer <token>"
}
data = {
    "model": "gemini-3-pro-image",
    "prompt": "A futuristic city with flying cars, cinematic lighting, 8k",
    "size": "16:9",
    "quality": "hd",
    "n": 1
}

response = requests.post(url, headers=headers, json=data)
print(response.json())
```

## 3. Image-to-Image (Edits) [New]

Call the `/v1/images/edits` endpoint to generate images from reference images.

*   **Content-Type**: `multipart/form-data`
*   **Multiple images supported**: you can upload several reference images at once.

### Form Field Reference

| Field Name | Type | Required | Description |
| :--- | :--- | :--- | :--- |
| `prompt` | String | Yes | text prompt |
| `image1`...`imageN` | File | Yes | **reference image files**. Supports file fields with any name such as `image1`, `image2`, etc. (not just the standard `image` or `mask`). |
| `image` | File | No | (OpenAI-standard compatible) primary image |
| `mask` | File | No | (OpenAI-standard compatible) mask image |
| `aspect_ratio` | String | No | explicitly specify the ratio, e.g. `"16:9"` (takes priority over `size`) |
| `image_size` | String | No | explicitly specify the resolution, e.g. `"2K"`, `"4K"` (takes priority over `quality`) |
| `style` | String | No | style description, automatically appended to the prompt |
| `n` | Integer | No | number of images to generate (default 1) |
| `model` | String | No | model name (default `gemini-3.1-flash-image`) |

### Call Example (Python)

```python
import requests

url = "http://localhost:8045/v1/images/edits"
headers = {
    "Authorization": "Bearer <token>"
}
# Multiple reference images supported (image1, image2, ...)
files = {
    "image1": open("/path/to/reference_1.jpg", "rb"),
    "image2": open("/path/to/reference_2.jpg", "rb")
}
data = {
    "prompt": "A cyberpunk city street based on this layout",
    "aspect_ratio": "16:9",
    "image_size": "4K",
    "style": "watercolor"
}

response = requests.post(url, headers=headers, files=files, data=data)
print(response.json())
```

---

## 4. Magic Suffix

In addition to the standard JSON parameters, this project also supports specifying parameters directly in the **model name** (convenient for clients that don't support custom parameters).

**Format**: `gemini-3-pro-image-{ratio}-{quality}`

*   **Ratio suffixes**: `-16x9`, `-9x16`, `-4x3`, `-3x4`, `-3x2`, `-2x3`, etc.
*   **Quality suffixes**: `-4k` (maps to hd), `-2k` (maps to medium).

**Example**:
using the model name `gemini-3-pro-image-16x9-4k` is equivalent to:
*   `size`: "1920x1080" (16:9)
*   `quality`: "hd"

> **Note**: if `size` or `quality` are explicitly passed in the JSON body, the body parameters take priority **over** the model name suffix.

---

## 5. FAQ

1.  **Q: Why did I set `size: "1234x5678"` but the generated image has the wrong ratio?**
    *   **A**: the system normalizes the size you enter to one of the 10 standard ratios Gemini supports (see section 2.1). If your ratio is unusual and doesn't match any standard ratio (tolerance > 0.05), the system falls back to the default **1:1**. It's recommended to use the resolutions from the examples directly.

2.  **Q: Can I generate multiple images in one call?**
    *   **A**: yes. Although the Gemini upstream limits a single request to generating 1 image, the Antigravity proxy layer automatically handles the `n` parameter with concurrency. For example, setting `n: 4` makes the system fire 4 requests in parallel and merge the results into the response.

3.  **Q: Getting an error with the `person_generation` parameter?**
    *   **A**: make sure this parameter is at the **root level** of the JSON (a sibling of `prompt`, `model`), not nested inside another field. Both `snake_case` (`person_generation`) and `camelCase` (`personGeneration`) are supported.
