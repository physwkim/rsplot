# rsplot — 설계 문서 (Design Doc v0)

egui + wgpu 위에 silx 스타일의 과학 시각화 라이브러리를 만든다. silx가
`BackendBase`(추상) ↔ `BackendOpenGL`/`BackendPygfx`(구현)로 렌더러를 갈아끼우듯,
이 라이브러리는 **고수준 Plot API ↔ `Backend` trait ↔ wgpu 렌더러**로 분리하고
wgpu 렌더러를 `egui_wgpu` paint callback 안에 끼워 넣는다.

이식의 1:1 레퍼런스는 트리 안의 두 파일이다:

- `silx/src/silx/gui/plot/backends/BackendBase.py` — 미러링할 추상 인터페이스
- `silx/src/silx/gui/plot/backends/BackendPygfx.py` — pygfx(=wgpu) 구현, 줄 단위 포팅 대상

> 본 문서의 결정: **1차 수직 슬라이스는 image+curve 공통 백엔드 동시**. 즉
> 공통 좌표계/`Backend` trait/wgpu 렌더 패스를 먼저 세우고 그 위에 `addImage`와
> `addCurve`를 같이 올린다.

---

## 0. 용어와 역할 매핑

| silx (Python/Qt/pygfx) | rsplot (Rust/egui/wgpu) |
|---|---|
| `PlotWidget` / `Plot1D` / `Plot2D` (고수준) | `Plot` + `PlotWidget` (egui widget) |
| `BackendBase` (추상 렌더 프리미티브) | `trait Backend` |
| `BackendPygfx` (pygfx/wgpu 구현) | `WgpuBackend` (`CallbackTrait` 구현) |
| Qt `QRenderWidget` 호스트 | egui `Ui` + eframe `RenderState` |
| pygfx 씬 그래프 (retained) | `callback_resources`에 사는 retained GPU state |
| pygfx `OrthographicCamera.show_rect` | uniform MVP 행렬 (ortho) |
| `silx.gui.colors.Colormap` | `Colormap` (256색 1D LUT + clim + norm) |
| GraphicFeature dirty-range 업로드 | `DirtyRange` 부분 `queue.write_buffer/texture` |

핵심 비대칭 하나: egui는 **immediate-mode**다. silx/fastplotlib는 retained 씬
그래프를 들고 있지만, 우리는 **데이터(GPU 버퍼·텍스처·파이프라인)만 프레임 간
유지**(`Renderer.callback_resources`, TypeMap)하고, egui UI 코드는 매 프레임
rect를 새로 할당해 paint callback을 다시 등록한다. callback 자체는
"이 plot을 그려라(+현재 transform)"만 담은 가벼운 값이고, 무거운 GPU state는
callback_resources에서 id로 조회한다.

---

## 1. 레이어 구조

```
┌─────────────────────────────────────────────────────────────┐
│ widget   egui 위젯: PlotWidget(ui).show(&mut plot)           │
│          - chrome 그리기(축/눈금/그리드/제목/컬러바/ROI 핸들) │
│          - 상호작용(pan/zoom/box-zoom/hover/pick) → 상태 갱신 │
│          - 데이터 영역 rect에 wgpu paint callback 등록        │
├─────────────────────────────────────────────────────────────┤
│ core     Plot 모델 + Backend trait + 공용 타입               │
│          - Plot: 아이템 목록, 축 상태(limits/log/inverted),  │
│            margins, title/labels, aspect-ratio               │
│          - trait Backend: addCurve/addImage/.../dataToPixel   │
│          - Colormap, Transform, ItemHandle, 좌표계            │
├─────────────────────────────────────────────────────────────┤
│ render   WgpuBackend: CallbackTrait 구현                     │
│          - GPU 리소스(파이프라인/버퍼/텍스처/LUT) 소유·유지   │
│          - prepare: transform uniform 갱신 + dirty 업로드     │
│          - paint:    set_viewport + 드로우 콜                 │
└─────────────────────────────────────────────────────────────┘
```

처음엔 단일 크레이트 내부 모듈(`core`/`render`/`widget`)로 두고, API가 안정되면
`rsplot-core` / `rsplot-wgpu` / `rsplot` 로 분리한다. `Backend`를 trait로
두는 이유는 silx와 동일: 고수준 API를 고정한 채 렌더러(테스트용 CPU mock, 다른
백엔드)를 갈아끼우기 위함이다.

---

## 2. `Backend` trait — `BackendBase` 미러

`BackendBase.py`의 공개 메서드를 Rust로 옮긴다. 1차 슬라이스에서 **반드시** 구현할
것에 ✅, 이후 마일스톤은 ◻︎.

```rust
/// 백엔드에 등록된 아이템 핸들. silx의 item handle과 동일 역할.
pub type ItemHandle = u64;

pub trait Backend {
    // ── 아이템 생성 ──────────────────────────────────────────
    ✅ fn add_curve(&mut self, curve: &CurveSpec) -> ItemHandle;   // BackendBase:90
    ✅ fn add_image(&mut self, image: &ImageSpec) -> ItemHandle;   // BackendBase:150
    ◻︎ fn add_triangles(&mut self, tris: &TriangleSpec) -> ItemHandle; // :168
    ◻︎ fn add_shape(&mut self, shape: &ShapeSpec) -> ItemHandle;    // :181
    ◻︎ fn add_marker(&mut self, marker: &MarkerSpec) -> ItemHandle; // :211
    ✅ fn remove(&mut self, item: ItemHandle);                     // :271

    // ── 축 / limits ──────────────────────────────────────────
    ✅ fn set_limits(&mut self, xmin: f64, xmax: f64,
                     ymin: f64, ymax: f64,
                     y2: Option<(f64, f64)>);                      // :410
    ✅ fn x_limits(&self) -> (f64, f64);                           // :425
    ✅ fn y_limits(&self, axis: YAxis) -> (f64, f64);              // :440
    ◻︎ fn set_x_log(&mut self, on: bool);                          // :491
    ◻︎ fn set_y_log(&mut self, on: bool);                          // :498
    ◻︎ fn set_x_inverted(&mut self, on: bool);                     // :505
    ◻︎ fn set_y_inverted(&mut self, on: bool);                     // :516
    ◻︎ fn set_keep_data_aspect_ratio(&mut self, on: bool);         // :535

    // ── 좌표 변환 (chrome ↔ 데이터 정합의 핵심) ──────────────
    ✅ fn data_to_pixel(&self, x: f64, y: f64, axis: YAxis) -> Option<Pos2>; // :553
    ✅ fn pixel_to_data(&self, p: Pos2, axis: YAxis) -> Option<(f64, f64)>;  // :569
    ✅ fn plot_bounds_in_pixels(&self) -> Rect;                    // :583
    ✅ fn set_axes_margins(&mut self, l: f32, t: f32, r: f32, b: f32); // :590

    // ── 라벨 / 색 ────────────────────────────────────────────
    ◻︎ fn set_title(&mut self, s: &str);                           // :386
    ◻︎ fn set_x_label(&mut self, s: &str);                         // :393
    ◻︎ fn set_y_label(&mut self, s: &str, axis: YAxis);            // :400
    ◻︎ fn set_foreground_colors(&mut self, fg: Color32, grid: Color32); // :602
    ◻︎ fn set_background_colors(&mut self, bg: Color32, data_bg: Color32); // :610

    // ── 피킹 ────────────────────────────────────────────────
    ◻︎ fn pick_item(&self, p: Pos2, item: ItemHandle) -> Option<PickResult>; // :335
    ◻︎ fn items_back_to_front(&self) -> Vec<ItemHandle>;           // :312

    // ── 라이프사이클 ─────────────────────────────────────────
    ✅ fn replot(&mut self);          // dirty 플래그 → 다음 프레임 재업로드 // :367
    ◻︎ fn save_graph(&self, path: &Path, fmt: ImageFormat, dpi: u32); // :372
}
```

`add_curve`/`add_image`는 1차 대상이므로 `BackendBase`의 전체 파라미터를 보존한다.

```rust
pub struct CurveSpec<'a> {            // BackendBase.addCurve (90-148)
    pub x: &'a [f64],
    pub y: &'a [f64],
    pub color: CurveColor,            // 단색 or per-vertex RGBA (N,4)
    pub gap_color: Option<Color32>,   // dashed gap 색
    pub symbol: Option<Symbol>,       // o . , + x d s
    pub line_width: f32,
    pub line_style: LineStyle,        // '-' '--' '-.' ':' none | custom dash
    pub y_axis: YAxis,                // Left | Right
    pub x_error: Option<ErrorBars>,
    pub y_error: Option<ErrorBars>,
    pub fill: bool,                   // baseline까지 채움
    pub alpha: f32,
    pub symbol_size: f32,
    pub baseline: Baseline,           // scalar | per-point
}

pub struct ImageSpec<'a> {            // BackendBase.addImage (150-166)
    pub data: ImageData<'a>,          // Scalar(&[f32], w, h) | Rgba(&[u8], w, h)
    pub origin: (f64, f64),           // 데이터 좌표 (ox, oy)
    pub scale: (f64, f64),            // 픽셀당 데이터 단위 (sx, sy)
    pub colormap: Option<Colormap>,   // scalar일 때만 적용 (RGBA는 무시)
    pub alpha: f32,
}
```

---

## 3. wgpu 렌더링 모델 — `egui_wgpu` 안에 끼우기

(근거: `egui/crates/egui-wgpu/src/renderer.rs:87` `CallbackTrait`,
`egui/crates/egui_demo_app/src/apps/custom3d_wgpu.rs` 패턴.)

### 3.1 GPU 리소스의 거처

모든 영속 GPU state는 eframe가 만든 `RenderState.renderer.write().callback_resources`
(타입맵)에 단 하나의 타입으로 넣는다. 여러 plot을 지원하기 위해 내부에
`HashMap<PlotId, PlotGpuState>`를 둔다.

```rust
struct WgpuResources {
    // 파이프라인은 plot 무관하게 공유
    image_pipeline: wgpu::RenderPipeline,
    line_pipeline:  wgpu::RenderPipeline,
    point_pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    plots: HashMap<PlotId, PlotGpuState>,   // plot별 데이터
}

struct PlotGpuState {
    transform: wgpu::Buffer,            // MVP uniform (ortho)
    images: HashMap<ItemHandle, GpuImage>,
    curves: HashMap<ItemHandle, GpuCurve>,
}

struct GpuImage {
    tiles: Vec<GpuImageTile>,           // max-texture-dim 초과 시 분할 (§6)
    lut:   wgpu::Texture,               // 256x1 RGBA8 컬러맵 LUT
    params: wgpu::Buffer,               // clim(vmin,vmax), norm mode, gamma, alpha
    bind_group: wgpu::BindGroup,
    dirty: DirtySet,                    // 데이터/LUT/params 중 무엇이 변했나
}
```

`WgpuResources`는 앱 시작 시(`CreationContext.wgpu_render_state`) 한 번 생성해
`callback_resources.insert()` 한다. 파이프라인 컴파일도 이때 1회.

### 3.2 callback 값과 3단 라이프사이클

UI 코드가 매 프레임 등록하는 callback은 가볍다:

```rust
struct PlotCallback {
    plot_id: PlotId,
    transform: Mat4,    // 이번 프레임의 데이터→NDC ortho 행렬 (현재 pan/zoom 반영)
    uploads: Vec<PendingUpload>,  // 이번 프레임에 반영할 dirty 데이터(있으면)
}

impl CallbackTrait for PlotCallback {
    // renderer.rs:88 — Device/Queue/Encoder 접근 가능. 업로드는 여기서.
    fn prepare(&self, device, queue, _desc, _enc, res: &mut CallbackResources)
        -> Vec<CommandBuffer>
    {
        let r: &mut WgpuResources = res.get_mut().unwrap();
        let plot = r.plots.get_mut(&self.plot_id).unwrap();
        queue.write_buffer(&plot.transform, 0, bytemuck::bytes_of(&self.transform));
        for up in &self.uploads { up.apply(device, queue, plot); } // dirty-range만
        Vec::new()
    }

    // renderer.rs:114 — 활성 RenderPass. 드로우만. set_viewport는 egui가 이미 해줌.
    fn paint(&self, info: PaintCallbackInfo, rp: &mut wgpu::RenderPass<'static>,
             res: &CallbackResources)
    {
        let r: &WgpuResources = res.get().unwrap();
        let plot = &r.plots[&self.plot_id];
        // back-to-front: 이미지 먼저, 곡선 위에
        for img in plot.images.values()  { img.draw(rp, &r.image_pipeline); }
        for cur in plot.curves.values()  { cur.draw(rp, &r.line_pipeline, &r.point_pipeline); }
    }
}
```

뷰포트 클리핑은 egui 쪽이 callback의 `rect`로부터 `set_viewport`를 호출해 준다
(`renderer.rs:567` 근방). 따라서 우리 셰이더는 **데이터 영역 rect = NDC 전체**로
간주하고 ortho 행렬만 맞추면 된다. 단, `PaintCallbackInfo.pixels_per_point`로
물리픽셀 변환이 필요한 계산(라인 두께 px 등)은 우리가 직접 처리한다
(`epaint/src/viewport.rs:4` `ViewportInPixels`).

### 3.3 immediate-mode와 retained의 결합 (dirty 흐름)

fastplotlib의 GraphicFeature dirty-range 패턴을 그대로 가져온다
(`fastplotlib/graphics/features/_base.py:135` BufferManager, `_update_range`).

1. 고수준 API (`plot.add_image(...)`, `image.set_data(...)`)가 `core` 모델을
   수정하고 해당 아이템에 **dirty range**를 기록한다 (전체 or 부분 슬라이스).
2. `PlotWidget::show()`가 프레임마다:
   - 상호작용 처리 → 축 limits/pan/zoom 갱신 → ortho `transform` 재계산
   - dirty 아이템을 모아 `PendingUpload`로 패키징
   - `Callback::new_paint_callback(data_rect, PlotCallback{..})` 등록
     (`renderer.rs:31`)
   - dirty 플래그 클리어
3. `prepare`에서 변경 구간만 `write_buffer`/`write_texture`. 변경 없으면 업로드 0,
   유지된 GPU 버퍼로 바로 `paint`.

즉 "씬 그래프는 매 프레임 새로, GPU 데이터는 유지, 바뀐 곳만 업로드". 라이브
파형/카메라 스트림에서 핵심 성능 특성이다.

---

## 4. 좌표계 — chrome와 셰이더의 단일 진실원

silx의 변환 사슬(`_PlotFrameCore.dataToPixel`, 1121-1172)을 그대로 따른다:
`data → (log/skew) → [bounds 정규화] → (margins/inverted) → pixel`.

데이터→NDC 변환은 **한 곳**(`Transform`)에서 만들고 두 소비자에게 동일하게 먹인다:

- **셰이더**: `Transform`에서 ortho MVP `Mat4`를 만들어 uniform으로. pygfx
  `camera.show_rect(xmin,xmax,ymin,ymax)`와 동치 (`BackendPygfx:1679`).
- **chrome(egui)**: 동일 limits/margins로 `emath::RectTransform::from_to(
  data_rect, pixel_rect)`를 만들어 축·눈금·마커·ROI 핸들을 egui `Painter`로 그린다
  (`egui/crates/emath/src/rect_transform.rs`). `data_to_pixel`/`pixel_to_data`는
  이 `RectTransform`(+log/inverted 후처리)으로 구현.

> MUST: 두 변환은 같은 (limits, margins, inverted, log, pixels_per_point)에서
> 파생되어야 한다. 두 곳에서 따로 계산하면 이미지와 축이 1px씩 어긋난다 — silx도
> 같은 함정을 `_PlotFrameCore` 단일 변환으로 막았다. 단일 owner = `Transform`.

축 종류는 `YAxis::{Left, Right}` (silx의 y/y2)와 X축. log/inverted/skew는
`Transform` 단계의 플래그로 처리하고 1차 슬라이스에서는 linear/non-inverted만 구현,
이후 마일스톤에서 켠다.

---

## 5. 컬러맵 모델

(근거: `BackendPygfx.py:365-480`, `fastplotlib/utils/functions.py:141-208`.)

```rust
pub struct Colormap {
    pub lut: [[u8; 4]; 256],    // 256색 RGBA8 → 256x1 GPU 텍스처
    pub norm: Normalization,
    pub vmin: Option<f64>,      // None = 데이터에서 autoscale(GPU min/max)
    pub vmax: Option<f64>,
    pub gamma: f32,
    pub nan_color: [u8; 4],
}

pub enum Normalization { Linear, Log, Sqrt, Gamma, Arcsinh }
```

- **셰이더 적용**: scalar 텍스처(f32 2D) + 1D LUT 텍스처. 셰이더가
  `t = clamp((value - vmin)/(vmax - vmin), 0, 1)` → `LUT[t]`. clim/gamma/norm은
  params uniform으로 전달. (pygfx `ImageBasicMaterial(map=lut, clim=...)` 동치,
  `BackendPygfx:1194`.)
- **정규화**: linear는 셰이더에서 바로. log/sqrt/arcsinh는 셰이더에서
  `value`를 변환하거나(권장) silx처럼 업로드 전 CPU 변환 후 clim 조정
  (`BackendPygfx:411-428`). gamma는 `1/gamma` 지수.
- **NaN sentinel**: silx 방식(`:435-480`) — NaN을 vmin 아래 sentinel로 치환,
  `LUT[0] = nan_color`, 원본 LUT는 1..255로 압축, clim 확장. 셰이더 `clamp`로
  sentinel이 index 0에 매핑. 1차 구현에서 채택.
- **컬러바**: 데이터 이미지와 **같은 LUT·clim·norm**으로 chrome에 별도 작은
  사각형 + 눈금. 이미지와 정확히 일치시키는 게 silx 정합성의 핵심.
- **카탈로그**: fastplotlib는 외부 `cmap` 라이브러리. Rust에서는 `colorous`
  또는 자체 테이블(viridis/plasma/inferno/magma/gray/jet 등 핵심 셋)로 시작.

---

## 6. 이미지 경로 (Plot2D)

(근거: `fastplotlib/graphics/features/_image.py:16-164` TextureArray 타일링,
`BackendPygfx` `_PygfxImageItem` 1023-1219.)

- scalar `f32` → `R32Float` 텍스처 1장. clim/LUT는 §5.
- **타일링**: `device.limits().max_texture_dimension_2d`(wgpu 기본 8192) 초과 시
  W/H를 청크 격자로 분할, 타일마다 텍스처 + 월드 오프셋. **LUT/params는 모든
  타일이 공유**(vmin/vmax/cmap 변경이 원자적으로 반영) — fastplotlib의 shared
  material과 동일 이유.
- 부분 갱신: `set_data` 시 변경 영역만 `write_texture`(dirty-range).
- `origin`/`scale`로 데이터 좌표에 배치 → quad 정점은 `Transform`이 NDC로.
- RGB(A) 이미지: LUT 없이 텍스처 직결.

---

## 7. 곡선 경로 (Plot1D)

(근거: `fastplotlib/graphics/line.py:25`, `scatter.py:25`,
`features/_positions.py:220`.)

- positions: `vec2<f32>` 버퍼(영속). per-point 갱신은 dirty-range 업로드.
- color: `UniformColor`(단색 uniform) vs `VertexColors`(per-vertex RGBA 버퍼) 분기.
  per-point colormap은 보조값을 LUT로 매핑(fastplotlib `VertexCmap`).
- line_width: 1차는 1px line-list, 두꺼운 선은 이후 quad-expansion 셰이더로.
- symbol(scatter): point_pipeline로 instanced quad + symbol SDF.
- **decimation**: fastplotlib엔 없다(전수 렌더). silx-급 대용량 파형은 별도
  마일스톤에서 x-bin별 min/max decimation을 GPU/CPU로 추가(요구 시).

---

## 8. 축·프레임·상호작용 (chrome, egui-side)

silx의 frame은 screen-space로 그려진다(`_PlotFrameCore`, `BackendPygfx`
`_updateFrame`/`_updateMarkers` 1644-1970). 우리는 이를 **egui `Painter`로 직접**
그린다(wgpu 아님): 축선/눈금/그리드/제목/라벨/범례/컬러바/마커/ROI 핸들.

- 눈금 위치 계산: nice-number 알고리즘(자체). log축은 데케이드 눈금.
- 상호작용: `ui.allocate_rect(data_rect, Sense::drag())` →
  - drag: pan (limits 평행이동)
  - scroll: zoom (커서 데이터좌표 고정 확대)
  - 박스 줌, 더블클릭 리셋, hover crosshair
- 피킹: 픽셀 ±3px 허용(silx `pickItem`, 2388-2553). 커서 픽셀을 `pixel_to_data`로
  데이터 박스로 바꿔 곡선/이미지 인덱스 산출.
- ROI/selector: fastplotlib처럼 "selector도 아이템", 선택 bounds는 상태이고
  egui 핸들 드래그로 갱신, 변경 시 콜백/이벤트 emit (이후 마일스톤).

---

## 9. 크레이트 구조 (초기: 단일 크레이트, 모듈 분리)

```
rsplot/
  Cargo.toml            # egui/eframe/egui-wgpu = 0.34, wgpu = 29, bytemuck
  src/
    lib.rs
    core/
      plot.rs           # Plot 모델, 축 상태, 아이템 목록, dirty 관리
      backend.rs        # trait Backend + 스펙 타입(CurveSpec/ImageSpec/...)
      transform.rs      # Transform: limits/margins → Mat4 + RectTransform
      colormap.rs       # Colormap, Normalization, 내장 LUT 테이블
      items.rs          # ItemHandle, Symbol, LineStyle, ErrorBars ...
    render/
      backend_wgpu.rs   # WgpuBackend: CallbackTrait + WgpuResources
      pipelines.rs      # image/line/point 파이프라인 생성
      gpu_image.rs      # GpuImage + 타일링 + LUT/params
      gpu_curve.rs      # GpuCurve
      shaders/*.wgsl
    widget/
      plot_widget.rs    # PlotWidget(ui).show(&mut plot): chrome+interaction+callback
      chrome.rs         # 축/눈금/그리드/컬러바 egui 드로잉
      interaction.rs    # pan/zoom/pick
  examples/
    image.rs            # 1차 슬라이스 데모: 이미지 + 컬러맵 + 컬러바
    curve.rs            # 1차 슬라이스 데모: 곡선 + 축
    image_and_curve.rs  # 둘 다 한 plot에
  doc/design.md         # 이 문서
```

eframe 앱 부트스트랩 시 `NativeOptions`에서 wgpu 백엔드를 선택하고
(`renderer: Renderer::Wgpu`), `CreationContext.wgpu_render_state`로 `WgpuResources`를
1회 초기화한다.

---

## 10. 버전 고정과 결합 (유지보수 세금)

- 워크스페이스 핀: **egui 0.34.3 / eframe 0.34.3 / egui-wgpu 0.34.3 / wgpu 29.0.1**,
  edition 2024, MSRV 1.92. 이 넷은 **반드시 한 버전축**으로 움직인다 — egui
  마이너 올릴 때 egui-wgpu·wgpu 동반 상향 필수. `Cargo.toml`에 정확 버전 핀.
- wgpu 29 → 셰이더는 WGSL, `RenderPass<'static>`(renderer.rs:114) 시그니처 준수.
- `bytemuck`으로 uniform POD 직렬화.

---

## 11. 1차 수직 슬라이스 — "image + curve 동시" 마일스톤

목표(verifiable): `cargo run --example image_and_curve`가 (a) 컬러맵+컬러바가
달린 2D scalar 이미지와 (b) 같은 plot 위 곡선을 동시에 띄우고, (c) pan/zoom 시
이미지·곡선·축이 1px 어긋남 없이 함께 움직인다.

순서:

1. **부트스트랩**: eframe + wgpu, 빈 `PlotWidget`이 데이터 rect에 단색 클리어
   콜백만 그림. `callback_resources`에 `WgpuResources` 삽입 검증.
2. **Transform 단일화**: `core::transform` — limits/margins → ortho `Mat4` +
   `RectTransform`. `data_to_pixel`/`pixel_to_data` 단위테스트(왕복 항등).
3. **이미지 파이프라인**: `R32Float` 텍스처 + 256x1 LUT + clim uniform 셰이더.
   `add_image` → quad 드로우. viridis LUT 내장. (타일링은 단일 텍스처 한도
   내에서는 생략, 한도 초과 케이스만 후속.)
4. **컬러바 + 축 chrome**: egui `Painter`로 축/눈금/그리드/컬러바. 이미지와
   동일 LUT·clim 사용 검증.
5. **곡선 파이프라인**: positions 버퍼 + 단색 line-list. `add_curve` → 드로우.
6. **상호작용**: pan/zoom/box-zoom/reset. transform 갱신이 셰이더·chrome 동시 반영.
7. **dirty-range 업로드**: `set_data`로 이미지/곡선 일부만 갱신, `prepare`에서
   부분 `write_*` 검증(라이브 갱신 데모).

이후 마일스톤(2차 페이즈): log/inverted/aspect-ratio, 마커/shape,
피킹, ROI selector, 대용량 decimation, 이미지 타일링, save_graph, y2 축,
두꺼운 라인/심볼 SDF, 컬러맵 카탈로그 확장. → **§13에서 의존성 순서와
단계별 검증 기준으로 단계화한다.**

---

## 12. 리스크 / 열린 질문

- **chrome ↔ 셰이더 정합**: §4의 단일 Transform 규칙을 깨면 재발하는 버그
  계열. 왕복 변환 단위테스트로 봉인.
- **pixels_per_point / DPI**: 라인 두께·심볼 크기·±3px 피킹은 물리픽셀 기준.
  `ViewportInPixels`로 일관 처리.
- **log축 정규화 위치** (결정됨, §13 A3): 좌표를 **업로드 전 CPU에서 log10
  변환**한다(silx와 동일). `Transform`은 축당 `scale ∈ {Linear, Log10}`을
  들고, 데이터 값 `v`를 정규화 `t∈[0,1]`로 보내는 단일 함수에서 log를
  적용한다(곡선/chrome 모두 같은 경로). ortho `Mat4`는 항상 정규화 공간을
  선형 매핑하므로 affine으로 충분하다. **이미지+log 한계**: 텍셀이 데이터
  등간격이라 log 공간에서 왜곡된다. 1차에는 곡선·축만 log를 지원하고,
  이미지+log는 프래그먼트 셰이더 역매핑으로 후속 처리한다(§13 A3 한계).
- **egui_plot 의존?**: 미사용 결정. chrome을 자체 구현해 wgpu transform과 강결합.
  필요하면 외부 `egui_plot` repo를 참고 구현으로만 본다.
- **멀티-plot**: `callback_resources`는 전역 TypeMap이라 `HashMap<PlotId,_>`로
  분리. PlotId 발급/회수(위젯 소멸 시 GPU 리소스 정리) 정책 필요.

---

## 13. 2차 페이즈 — 백로그 단계 계획

§11 슬라이스 1(1~7단계) 완료 후의 백로그를 의존성 순서로 웨이브화한다.
규칙은 1차와 동일: 코드 주석 영어, 항목마다 fmt/clippy/nextest 통과 + 단계별
커밋, 가능한 한 순수 함수에 단위테스트. 각 항목에 "done = …" 검증 기준을 둔다.

### Wave A — 좌표계 일반화 (`core::transform`; 가장 많은 기능이 의존)

단일 진실원 규칙(§4)을 유지하기 위해, 축을 `Axis { min, max, scale, inverted }`
로 일반화한다(`scale ∈ {Linear, Log10}`). 데이터 값 `v`를 정규화 `t∈[0,1]`로
보내는 **단일 함수** `norm(axis, v)`와 그 역 `denorm`을 두고, `data_to_pixel` /
`pixel_to_data` / `ortho_matrix`가 전부 이 한 경로에서 파생되게 한다.

- **A1. 축 모델 일반화**: 위 `Axis`/`norm` 도입, 기존 linear 동작 보존.
  done = 기존 transform 단위테스트 전부 통과 + `norm`/`denorm` 왕복 항등 테스트.
- **A2. inverted 축 (X/Y)**: 정규화에서 `t → 1−t`. affine이라 ortho로도 표현됨.
  done = 뒤집힌 축에서 이미지·곡선·눈금이 1px 정합.
- **A3. log 축 (X/Y)**: `norm`이 log10 적용(`min>0` 전제), chrome은 데케이드
  눈금. 좌표는 CPU에서 변환(§12 결정). done = 로그 축 곡선 + 데케이드 눈금
  데모. **한계**: 이미지+log는 텍셀 왜곡 때문에 후속(프래그먼트 역매핑).
- **A4. aspect-ratio lock**: 데이터 단위를 정사각으로 유지하도록 한계를 확장
  보정(이미지 왜곡 방지). done = 정사각 픽셀에서 원이 원으로 보임.
- **A5. y2 축**: 두 번째 Y 한계 + 우측 눈금, 곡선이 `y`/`y2`에 바인딩.
  done = 좌/우 다른 스케일의 두 곡선이 각자 축에 정합.

### Wave B — 1D 비주얼 (`render::gpu_curve` + 새 점 파이프라인)

- **B1. 두꺼운 라인**: line-strip → quad-expansion(triangle-strip) 셰이더,
  픽셀 폭 uniform(물리픽셀 기준, §12 DPI). done = 폭 ≥1px 가변 라인.
  **라운드 join/cap 완료**: 각 정점에 선 폭 지름의 AA 디스크를 찍어
  (`shaders/linecaps.wgsl`, `GpuCurve::draw_caps`) 꺾임의 바깥쪽 틈을 메우고
  끝을 둥글게 — silx pygfx `LineMaterial` 기본값. 실선 전용(점선은 butt 유지),
  반투명 선은 세그먼트와 겹쳐 합성되는 한계(불투명은 정확). GPU readback 검증
  (`tests/curve_caps_joins.rs`).
- **B2. 마커/심볼**: 점 인스턴싱 + SDF 프래그먼트(circle/square/cross/plus/
  triangle). done = 곡선 정점에 심볼, 크기 픽셀 기준.

### Wave C — 상호작용/피킹 (`widget::interaction`)

- **C1. hover crosshair**: 데이터 좌표 십자선 + 좌표 라벨.
- **C2. 피킹(±3px)**: pixel→data 박스로 곡선 최근접 점/이미지 인덱스 산출,
  결과를 `Response`/콜백으로 반환. done = 클릭 지점의 점/픽셀 인덱스 정확.
- **C3. ROI selector**: 사각/수평/수직 영역 아이템 + egui 핸들 드래그, 변경
  이벤트 emit. done = 핸들 드래그로 bounds 갱신·이벤트 수신.

### Wave D — 스케일/대용량

- **D1. decimation**: 큰 곡선을 픽셀 칼럼당 min/max로 다운샘플해 드로우(시각
  동일, 정점 수 ↓). done = N≫픽셀에서 시각 동일 + 정점 감소 측정.
- **D2. 이미지 타일링**: `max_texture_dimension_2d` 초과 이미지를 타일 분할
  업로드/드로우. done = 한도 초과 이미지가 정상 표시.

### Wave E — 운영

- **E1. save_graph(PNG)**: 오프스크린 타깃 렌더 후 리드백 저장.
  done = 현재 뷰가 PNG로 저장됨.
- **E2. 컬러맵 카탈로그**: viridis 외 magma/inferno/plasma/cividis/gray 등 +
  reverse. done = 이름으로 컬러맵 선택 + 컬러바 반영.

순서 메모: A가 토대(B/C가 좌표·픽셀폭에 의존), E2는 독립적이라 어느 시점에나
가능(워밍업으로 먼저 처리). 이미지+log, ROI 전체 종류는
각 항목 내 후속으로 표시한다. (두꺼운 라인 join/cap는 B1에서 완료.)

---

## 부록 A. BackendPygfx 포팅 앵커 (줄 단위)

| 기능 | BackendPygfx.py | 이식 위치 |
|---|---|---|
| addCurve 아이템 | 115-299 | `render::gpu_curve` |
| addImage 아이템 | 1023-1219 | `render::gpu_image` |
| 컬러맵 LUT/정규화/NaN | 365-480 | `core::colormap` + 셰이더 |
| dataToPixel/pixelToData | (_PlotFrameCore) 1121-1219 | `core::transform` |
| ortho 카메라 show_rect | 1517, 1679-1707 | `core::transform`(Mat4) |
| 렌더 루프(2-pass) | 1644-1813 | `widget`(chrome) + `render`(data) |
| 마커 screen-space | 1814-1970 | `widget::chrome` |
| 피킹 dispatch | 2388-2553 | `widget::interaction` |
| 이미지 풀링/재사용 | 2231-2242 | `WgpuResources` 핸들 맵 |
