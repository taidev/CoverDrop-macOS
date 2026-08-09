import React, { useEffect, useMemo, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Check, ChevronLeft, ChevronRight, FileImage, FolderOpen, HelpCircle, Image, LoaderCircle, Moon, Plus, Settings2, Sun, Upload, X } from "lucide-react";
import "./styles.css";

type Format = "jpeg" | "png" | "webp" | "gif";
type Unit = "px" | "in" | "cm" | "mm" | "pt";
type SourceMode = "files" | "folder";
type ResizeMode = "contain" | "width" | "height" | "crop";
type CropAnchor = "top-left" | "top" | "top-right" | "left" | "center" | "right" | "bottom-left" | "bottom" | "bottom-right";
type Theme = "light" | "dark";
type Quality = "low" | "medium" | "high" | "very-high" | "maximum";
type Settings = { format: Format; unit: Unit; width: number | null; height: number | null; ppi: number; resizeMode: ResizeMode; cropAnchor: CropAnchor; quality: Quality };
type ItemResult = { source: string; output?: string; error?: string };
type Summary = { completed: ItemResult[]; failed: ItemResult[] };
type PreviewResponse = { path: string; cacheHit: boolean };
const formats: Format[] = ["jpeg", "png", "webp", "gif"];
const formatLabel: Record<Format, string> = { jpeg: "JPEG", png: "PNG", webp: "WebP", gif: "GIF" };
const baseSettings: Settings = { format: "jpeg", unit: "px", width: null, height: null, ppi: 150, resizeMode: "contain", cropAnchor: "center", quality: "high" };
const cropAnchors: { value: CropAnchor; label: string }[] = [
  { value: "top-left", label: "Top left" }, { value: "top", label: "Top" }, { value: "top-right", label: "Top right" },
  { value: "left", label: "Left" }, { value: "center", label: "Center" }, { value: "right", label: "Right" },
  { value: "bottom-left", label: "Bottom left" }, { value: "bottom", label: "Bottom" }, { value: "bottom-right", label: "Bottom right" }
];
const nameOf = (path: string) => path.split(/[\\/]/).pop() || path;

function App() {
  const [mode, setMode] = useState<SourceMode>("files");
  const [files, setFiles] = useState<string[]>([]);
  const [folder, setFolder] = useState("");
  const [folderFiles, setFolderFiles] = useState<string[]>([]);
  const [outputDir, setOutputDir] = useState("");
  const [settings, setSettings] = useState<Settings>(baseSettings);
  const [activeFile, setActiveFile] = useState("");
  const [preview, setPreview] = useState("");
  const [previewLoading, setPreviewLoading] = useState(false);
  const [working, setWorking] = useState(false);
  const [progress, setProgress] = useState<{ current: number; total: number; file: string } | null>(null);
  const [summary, setSummary] = useState<Summary | null>(null);
  const [theme, setTheme] = useState<Theme>(() => localStorage.getItem("coverdrop-theme") === "dark" ? "dark" : "light");
  const [helpOpen, setHelpOpen] = useState(false);
  const previewRequestId = useRef(0);
  const queue = mode === "files" ? files : folderFiles;
  const ready = Boolean(outputDir && queue.length);
  const activeIndex = Math.max(0, queue.indexOf(activeFile));
  const canCrop = Boolean(settings.width && settings.height);
  const hasCustomSize = Boolean(settings.width || settings.height);
  const update = <K extends keyof Settings>(key: K, value: Settings[K]) => setSettings((previous) => ({ ...previous, [key]: value }));
  const showError = (source: string, error: unknown) => setSummary({ completed: [], failed: [{ source, error: String(error) }] });

  useEffect(() => {
    invoke<Settings>("get_settings").then(setSettings).catch(() => undefined);
    let unlisten: (() => void) | undefined;
    listen<{ current: number; total: number; file: string }>("conversion-progress", (event) => setProgress(event.payload)).then((stop) => { unlisten = stop; });
    return () => unlisten?.();
  }, []);
  useEffect(() => { document.documentElement.dataset.theme = theme; localStorage.setItem("coverdrop-theme", theme); }, [theme]);
  useEffect(() => {
    if (!activeFile) { setPreview(""); setPreviewLoading(false); return; }
    const requestId = ++previewRequestId.current;
    setPreviewLoading(true); setPreview("");
    let cancelled = false;
    invoke<PreviewResponse>("get_pdf_preview", { path: activeFile, requestId, prefetch: false })
      .then((result) => {
        if (cancelled) return;
        setPreview(convertFileSrc(result.path));
        const index = queue.indexOf(activeFile);
        const neighbors = [queue[index + 1], queue[index - 1]].filter(Boolean);
        void (async () => {
          for (const path of neighbors) {
            try {
              const prefetchId = ++previewRequestId.current;
              const prefetched = await invoke<PreviewResponse>("get_pdf_preview", { path, requestId: prefetchId, prefetch: true });
              const image = new window.Image(); image.src = convertFileSrc(prefetched.path);
            } catch { /* Preloading is opportunistic and never blocks the active preview. */ }
          }
        })();
      })
      .catch((error) => { if (!cancelled && !String(error).includes("cancelled")) showError("Preview", error); })
      .finally(() => { if (!cancelled) setPreviewLoading(false); });
    return () => { cancelled = true; void invoke("cancel_pdf_preview", { requestId }); };
  }, [activeFile, queue]);
  useEffect(() => { if (queue.length && !queue.includes(activeFile)) setActiveFile(queue[0]); if (!queue.length) setActiveFile(""); }, [queue, activeFile]);

  async function chooseFiles(append = false) {
    try { const picked = await open({ multiple: true, filters: [{ name: "PDF documents", extensions: ["pdf"] }] }); if (Array.isArray(picked) && picked.length) { const next = append ? [...files, ...picked.filter((path) => !files.includes(path))] : picked; setMode("files"); setFiles(next); setActiveFile(next[0]); } } catch (error) { showError("File picker", error); }
  }
  async function chooseFolder() {
    try { const picked = await open({ directory: true, multiple: false }); if (typeof picked === "string") { const found = await invoke<string[]>("list_pdfs_in_folder", { folder: picked }); setMode("folder"); setFolder(picked); setFolderFiles(found); setActiveFile(found[0] || ""); } } catch (error) { showError("Folder picker", error); }
  }
  async function chooseOutput() { try { const picked = await open({ directory: true, multiple: false, title: "Choose where to save cover images" }); if (typeof picked === "string") setOutputDir(picked); } catch (error) { showError("Save-location picker", error); } }
  async function openTaiDev() { try { await openUrl("https://taidev.org"); } catch (error) { showError("TaiDev website", error); } }
  async function generate() { if (!ready || working) return; setWorking(true); setSummary(null); setProgress(null); try { await invoke("save_settings", { settings }); setSummary(await invoke<Summary>("generate_covers", { job: { sourceMode: mode, files, folder: folder || null, outputDir, settings } })); } catch (error) { showError("Conversion", error); } finally { setWorking(false); setProgress(null); } }
  function removeFile(path: string) { const next = files.filter((file) => file !== path); setFiles(next); if (activeFile === path) setActiveFile(next[0] || ""); }
  const previewSize = settings.width && settings.height ? `${settings.width} × ${settings.height} ${settings.unit}` : settings.width ? `${settings.width} ${settings.unit} wide` : settings.height ? `${settings.height} ${settings.unit} high` : "Original size";

  return <main className="studio">
    <aside className="sidebar">
      <div><div className="brand"><span><img src="/coverdrop-logo.png" alt="coverDrop" /></span><div><strong><span>cover</span><b>Drop</b></strong><small>PDF cover extractor</small></div></div>
        <section className="queue"><div className="queue-head"><strong>Selected PDFs <em>{queue.length}</em></strong><button onClick={() => mode === "files" ? chooseFiles(true) : chooseFolder()}><Plus size={14}/> Add more</button></div>{queue.length ? <div className="queue-list">{queue.map((file, index) => <button className={file === activeFile ? "queue-item active" : "queue-item"} onClick={() => setActiveFile(file)} key={file}><span className="mini-cover"><FileImage size={17}/></span><span><strong>{nameOf(file)}</strong><small>{file === activeFile ? "Preview selected" : `PDF ${index + 1}`}</small></span>{mode === "files" ? <i onClick={(event) => { event.stopPropagation(); removeFile(file); }}><X size={13}/></i> : null}</button>)}</div> : <button className="empty-queue" onClick={() => chooseFiles()}><Upload size={18}/><span>Add PDF files to begin</span></button>}</section>
      </div>
      <div className="sidebar-bottom"><button className="icon-button" onClick={() => setTheme(theme === "light" ? "dark" : "light")} title="Switch theme">{theme === "light" ? <Moon size={18}/> : <Sun size={18}/>}</button><button className="icon-button" onClick={() => setHelpOpen(true)} title="Help"><HelpCircle size={18}/></button></div>
    </aside>
    <section className="preview-column"><header className="preview-header"><div className="preview-heading"><Image size={18}/><div><strong>Preview</strong><span>{queue.length ? `Cover ${activeIndex + 1} of ${queue.length}` : "First page cover preview"}</span></div></div>{queue.length > 1 ? <div className="pager"><button disabled={activeIndex === 0} onClick={() => setActiveFile(queue[activeIndex - 1])}><ChevronLeft size={18}/></button><b>{activeIndex + 1} / {queue.length}</b><button disabled={activeIndex === queue.length - 1} onClick={() => setActiveFile(queue[activeIndex + 1])}><ChevronRight size={18}/></button></div> : null}</header>
      <div className="preview-stage">{previewLoading ? <LoaderCircle className="spin" size={28}/> : preview ? <>{(settings.width || settings.height) ? <span className="size-badge top">{previewSize}</span> : null}<div className={settings.resizeMode === "crop" && canCrop ? "cover-frame crop-active" : "cover-frame"}><img src={preview} alt={`Preview of ${nameOf(activeFile)}`}/></div>{settings.resizeMode === "crop" && canCrop ? <span className="crop-label">Crop preview · {cropAnchors.find((anchor) => anchor.value === settings.cropAnchor)?.label}</span> : null}</> : <div className="preview-empty"><Image size={36}/><strong>Your selected cover will appear here</strong><span>Choose one or more PDF files to preview page one.</span></div>}</div>
      <div className="preview-meta"><span>{activeFile ? nameOf(activeFile) : "No PDF selected"}</span></div>
    </section>
    <aside className="inspector"><section className="inspector-body"><div className="inspector-heading"><Settings2 size={18}/><div><strong>Export settings</strong><span>Choose the output size and format.</span></div></div><div className="format-row"><label>Format<select value={settings.format} onChange={(event) => update("format", event.target.value as Format)}>{formats.map((format) => <option key={format} value={format}>{formatLabel[format]}</option>)}</select></label><label>Units<select value={settings.unit} onChange={(event) => update("unit", event.target.value as Unit)}><option value="px">px</option><option value="in">in</option><option value="cm">cm</option><option value="mm">mm</option><option value="pt">pt</option></select></label></div>{(settings.format === "jpeg" || settings.format === "webp") ? <label className="quality-field">Image quality<select value={settings.quality} onChange={(event) => update("quality", event.target.value as Quality)}><option value="low">Low</option><option value="medium">Medium</option><option value="high">High (Recommended)</option><option value="very-high">Very High</option><option value="maximum">Maximum</option></select></label> : null}<div className="dimension-fields"><label>Width<span><input inputMode="decimal" step="any" placeholder="Auto" value={settings.width ?? ""} onChange={(event) => update("width", event.target.value ? Number(event.target.value) : null)}/><b>{settings.unit}</b></span></label><label>Height<span><input inputMode="decimal" step="any" placeholder="Auto" value={settings.height ?? ""} onChange={(event) => update("height", event.target.value ? Number(event.target.value) : null)}/><b>{settings.unit}</b></span></label></div><p className="inspector-note">{settings.format === "webp" ? "WebP quality balances file size and visual detail." : "Leave a dimension empty to preserve the cover’s original proportion."}</p>{hasCustomSize ? <div className="crop-section"><div className="crop-heading"><Settings2 size={16}/><strong>Crop options</strong></div><label>Sizing</label><div className="fit-toggle"><button className={settings.resizeMode !== "crop" ? "active" : ""} onClick={() => update("resizeMode", "contain")}><strong>Fit</strong><small>Scale to fit</small></button><button className={settings.resizeMode === "crop" ? "active" : ""} disabled={!canCrop} onClick={() => update("resizeMode", "crop")}><strong>Fill</strong><small>Fill and crop</small></button></div><label>Crop anchor</label><div className="anchor-grid">{cropAnchors.map((anchor) => <button key={anchor.value} aria-label={anchor.label} title={anchor.label} className={settings.cropAnchor === anchor.value ? "active" : ""} onClick={() => update("cropAnchor", anchor.value)}><i/></button>)}</div><button className="reset-crop" onClick={() => { update("resizeMode", "contain"); update("cropAnchor", "center"); }}>Reset crop</button><p className="inspector-note">{canCrop ? "Crop starts from the selected anchor position." : "Enter a width and height to enable crop."}</p></div> : null}</section>
      <div className="output-path"><div className="output-path-head"><strong>Save to</strong></div><button className="output-location" onClick={chooseOutput} aria-label="Choose a save location"><FolderOpen size={16}/><span title={outputDir}>{outputDir || "Choose a save location"}</span><b>Browse</b></button></div>
      <button className="generate" disabled={!ready || working} onClick={generate}>{working ? <LoaderCircle className="spin" size={20}/> : <svg className="generate-icon" fill="none" viewBox="0 0 64 64" aria-hidden="true"><g clipRule="evenodd" fill="currentColor" fillRule="evenodd"><path d="M47 2c.9337 0 1.7432.64607 1.9502 1.55654l1.1066 4.8664c.5118 2.25086 2.2694 4.00846 4.5203 4.52026l4.8664 1.1066c.9104.207 1.5565 1.0165 1.5565 1.9502s-.6461 1.7432-1.5565 1.9502l-4.8664 1.1066c-2.2509.5118-4.0085 2.2694-4.5203 4.5203l-1.1066 4.8664c-.207.9104-1.0165 1.5565-1.9502 1.5565s-1.7432-.6461-1.9502-1.5565l-1.1066-4.8664c-.5118-2.2509-2.2694-4.0085-4.5203-4.5203l-4.8664-1.1066c-.9104-.207-1.5565-1.0165-1.5565-1.9502s.6461-1.7432 1.5565-1.9502l4.8664-1.1066c2.2509-.5118 4.0085-2.2694 4.5203-4.52026l1.1066-4.8664C45.2568 2.64607 46.0663 2 47 2Z"/><path d="M35.188 23c-3.011 0-5.5 2.4358-5.5 5.5s2.489 5.5 5.5 5.5c3.0111 0 5.5-2.4358 5.5-5.5s-2.4889-5.5-5.5-5.5Z"/><path d="M11 14h19.417c-.2692.6162-.417 1.2939-.417 2s.1478 1.3838.417 2H11c-2.20914 0-4 1.7909-4 4v17.0217c8.8025-2.2267 17.6822.1448 24.1162 5.6382 4.2507-2.7198 9.2619-3.3954 13.8838-2.2544v-9.8225c.6162.2692 1.2939.417 2 .417s1.3838-.1478 2-.417v12.547c.0007.0301.0007.0603 0 .0905v8.7795c0 4.4183-3.5817 8-8 8H11c-4.41828 0-8-3.5817-8-8V22c0-4.4183 3.58172-8 8-8Z"/></g></svg>} {working ? "Generating..." : `Generate ${queue.length || ""} cover${queue.length === 1 ? "" : "s"}`}</button>
      {working && progress ? <div className="progress"><span>{progress.file}</span><i style={{ width: `${(progress.current / progress.total) * 100}%` }}/></div> : null}{summary ? <section className={`summary ${summary.failed.length ? "has-errors" : ""}`}><Check size={18}/><span>{summary.failed.length ? summary.failed[0].error : `${summary.completed.length} cover images created`}</span></section> : null}
    </aside>
    <footer><span>Developed by</span><button onClick={openTaiDev}>TaiDev</button></footer>
    {helpOpen ? <div className="help-backdrop" onMouseDown={() => setHelpOpen(false)}><section className="help-dialog" role="dialog" aria-modal="true" aria-labelledby="help-title" onMouseDown={(event) => event.stopPropagation()}><header><span className="help-icon"><HelpCircle size={21}/></span><div><h2 id="help-title">Quick start</h2><p>Extract first-page covers in a few steps.</p></div><button className="help-close" onClick={() => setHelpOpen(false)} aria-label="Close help"><X size={18}/></button></header><ol className="help-steps"><li><b>1</b><div><strong>Add PDFs</strong><span>Choose individual PDF files or a folder. The first page becomes the cover preview.</span></div></li><li><b>2</b><div><strong>Choose where to save</strong><span>Select an output folder. Folder batches preserve the source subfolder structure.</span></div></li><li><b>3</b><div><strong>Set output options</strong><span>Choose a format, size, and quality. Leave one dimension blank to keep the original proportion.</span></div></li></ol><div className="help-tip"><strong>Useful tip</strong><p>Use Fill and Crop only after entering both width and height. Choose an anchor to control which part of the cover is kept.</p></div><button className="help-done" onClick={() => setHelpOpen(false)}>Got it</button></section></div> : null}
  </main>;
}
createRoot(document.getElementById("root")!).render(<App />);
