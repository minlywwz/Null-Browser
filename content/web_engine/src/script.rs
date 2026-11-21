use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
    time::{Duration, Instant},
};

use rquickjs::{Context, Ctx, Exception, Function, Persistent, Runtime};

use crate::{
    Viewport,
    dom::{Document, NodeId},
    html_parser::replace_children,
};

const MAX_SOURCE: usize = 1024 * 1024;
const MAX_TASKS: usize = 64;

thread_local! {
    static PAGE: RefCell<Option<Rc<RefCell<Page>>>> = const { RefCell::new(None) };
}

struct Page {
    document: Document,
    sources: HashMap<String, String>,
    clicks: Vec<(NodeId, Persistent<Function<'static>>)>,
    tasks: Vec<Persistent<Function<'static>>>,
    executed: HashSet<NodeId>,
    errors: Vec<String>,
}

impl Drop for ScriptSession {
    fn drop(&mut self) {
        self.context.with(|ctx| {
            let Ok(mut page) = self.page.try_borrow_mut() else {
                return;
            };
            for (_, function) in page.clicks.drain(..) {
                drop(function.restore(&ctx).ok());
            }
            for function in page.tasks.drain(..) {
                drop(function.restore(&ctx).ok());
            }
        });
    }
}

pub struct ScriptSession {
    runtime: Runtime,
    context: Context,
    page: Rc<RefCell<Page>>,
    started: bool,
}

impl ScriptSession {
    pub fn open(html: &str) -> Result<Self, String> {
        let document = crate::parse_html(html)?;
        let runtime = Runtime::new().map_err(|error| error.to_string())?;
        runtime.set_memory_limit(32 * 1024 * 1024);
        runtime.set_max_stack_size(512 * 1024);
        let context = Context::full(&runtime).map_err(|error| error.to_string())?;
        let page = Rc::new(RefCell::new(Page {
            document,
            sources: HashMap::new(),
            clicks: Vec::new(),
            tasks: Vec::new(),
            executed: HashSet::new(),
            errors: Vec::new(),
        }));
        let session = Self {
            runtime,
            context,
            page,
            started: false,
        };
        session.install_host()?;
        Ok(session)
    }

    pub fn requested_scripts(&self) -> Vec<String> {
        let document = self.document();
        let mut scripts = document.external_scripts();
        for block in document.script_blocks() {
            if block.src.is_none() {
                scripts.extend(module_imports(&block.code));
            }
        }
        scripts
    }

    pub fn document(&self) -> std::cell::Ref<'_, Document> {
        std::cell::Ref::map(self.page.borrow(), |page| &page.document)
    }

    pub fn document_mut(&self) -> std::cell::RefMut<'_, Document> {
        std::cell::RefMut::map(self.page.borrow_mut(), |page| &mut page.document)
    }

    pub fn install_scripts(&mut self, scripts: impl IntoIterator<Item = (String, String)>) {
        let mut page = self.page.borrow_mut();
        for (reference, source) in scripts {
            if reference.len() <= 8 * 1024 && source.len() <= MAX_SOURCE {
                page.sources.insert(reference, source);
            }
        }
    }

    pub fn run(&mut self, viewport: Viewport) -> Result<(), String> {
        if self.started {
            return Ok(());
        }
        self.started = true;
        self.execute(viewport, true)
    }

    pub fn click(&mut self, node: u32, viewport: Viewport) -> Result<(), String> {
        let node = node as usize;
        self.with_page(viewport, |ctx| {
            let planned = {
                let page = self.page.borrow();
                let mut planned = Vec::new();
                for id in ancestor_chain(&page.document, node) {
                    for listener in &page.clicks {
                        if listener.0 == id {
                            planned.push(listener.clone());
                        }
                    }
                }
                planned
            };
            for listener in planned {
                let function = listener
                    .1
                    .clone()
                    .restore(&ctx)
                    .map_err(|error| error.to_string())?;
                let event =
                    rquickjs::Object::new(ctx.clone()).map_err(|error| error.to_string())?;
                event
                    .set("type", "click")
                    .map_err(|error| error.to_string())?;
                if let Err(error) = function.call::<_, rquickjs::Value>((event,)) {
                    self.page.borrow_mut().errors.push(explain(&ctx, error));
                }
                drain_tasks(&ctx, &mut self.page.borrow_mut());
            }
            Ok(())
        })?;
        self.execute(viewport, false)
    }

    pub fn failure(&self) -> Option<String> {
        let page = self.page.borrow();
        if page.errors.is_empty() {
            None
        } else {
            Some(page.errors.join("\n"))
        }
    }

    fn execute(&mut self, viewport: Viewport, initial: bool) -> Result<(), String> {
        self.with_page(viewport, |ctx| {
            if initial {
                let mut page = self.page.borrow_mut();
                page.document.scripting = !page.document.script_blocks().is_empty();
                let referenced: HashSet<String> =
                    page.document.external_scripts().into_iter().collect();
                let dependencies: Vec<(String, String)> = page
                    .sources
                    .iter()
                    .filter(|(key, _)| !referenced.contains(*key))
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect();
                drop(page);
                for (name, source) in dependencies {
                    let code = wrap_module(&strip_if_needed(&source), &name);
                    if let Err(error) = ctx.eval::<(), _>(code) {
                        self.page.borrow_mut().errors.push(explain(&ctx, error));
                    }
                }
            }
            for _ in 0..4 {
                let pending: Vec<_> = {
                    let page = self.page.borrow();
                    page.document
                        .script_blocks()
                        .into_iter()
                        .filter(|script| !page.executed.contains(&script.id))
                        .collect()
                };
                if pending.is_empty() {
                    break;
                }
                {
                    let mut page = self.page.borrow_mut();
                    for script in &pending {
                        page.executed.insert(script.id);
                    }
                }
                let mut modules = Vec::new();
                for script in &pending {
                    let prepared = {
                        let page = self.page.borrow();
                        prepare_script(&page, script)
                    };
                    let Some(prepared) = prepared else {
                        continue;
                    };
                    if prepared.module {
                        modules.push(prepared);
                    } else if let Err(error) = ctx.eval::<(), _>(prepared.code) {
                        self.page.borrow_mut().errors.push(explain(&ctx, error));
                    } else {
                        drain_tasks(&ctx, &mut self.page.borrow_mut());
                    }
                }
                for module in &modules {
                    if let Err(error) = ctx.eval::<(), _>(module.code.as_str()) {
                        self.page.borrow_mut().errors.push(explain(&ctx, error));
                    }
                }
                for module in &modules {
                    let load = format!("__ko4mod({})", js_string(&module.name));
                    if let Err(error) = ctx.eval::<(), _>(load) {
                        self.page.borrow_mut().errors.push(explain(&ctx, error));
                    } else {
                        drain_tasks(&ctx, &mut self.page.borrow_mut());
                    }
                }
            }
            Ok(())
        })
    }

    fn install_host(&self) -> Result<(), String> {
        self.context.with(|ctx| {
            let host = rquickjs::Object::new(ctx.clone()).map_err(|error| error.to_string())?;
            bind(&ctx, &host)?;
            ctx.globals()
                .set("__host", host)
                .map_err(|error| error.to_string())?;
            ctx.eval::<(), _>(BOOTSTRAP)
                .map_err(|error| explain(&ctx, error))?;
            Ok(())
        })
    }

    fn with_page<T>(
        &self,
        viewport: Viewport,
        body: impl FnOnce(Ctx<'_>) -> Result<T, String>,
    ) -> Result<T, String> {
        let started = Instant::now();
        self.runtime.set_interrupt_handler(Some(Box::new(move || {
            started.elapsed() > Duration::from_millis(800)
        })));
        let result = self.context.with(|ctx| {
            let _guard = PageGuard::enter(self.page.clone());
            let _ = ctx.globals().set("innerWidth", viewport.width as f64);
            let _ = ctx.globals().set("innerHeight", viewport.height as f64);
            body(ctx)
        });
        self.runtime.set_interrupt_handler(None);
        result
    }
}

struct PageGuard;

impl PageGuard {
    fn enter(page: Rc<RefCell<Page>>) -> Self {
        PAGE.with(|slot| *slot.borrow_mut() = Some(page));
        Self
    }
}

impl Drop for PageGuard {
    fn drop(&mut self) {
        PAGE.with(|slot| *slot.borrow_mut() = None);
    }
}

fn with_host<T>(
    ctx: &Ctx<'_>,
    body: impl FnOnce(&mut Page) -> Result<T, String>,
) -> rquickjs::Result<T> {
    PAGE.with(|slot| {
        let Some(page) = slot.borrow().as_ref().cloned() else {
            return Err(Exception::throw_internal(ctx, "The page is not active."));
        };
        let mut page = page
            .try_borrow_mut()
            .map_err(|_| Exception::throw_type(ctx, "The document is busy."))?;
        body(&mut page).map_err(|message| Exception::throw_type(ctx, &message))
    })
}

fn bind<'js>(ctx: &Ctx<'js>, host: &rquickjs::Object<'js>) -> Result<(), String> {
    let by_id = Function::new(
        ctx.clone(),
        |ctx: Ctx<'_>, id: String| -> rquickjs::Result<i32> {
            with_host(&ctx, |page| {
                Ok(page
                    .document
                    .element_by_id(&id)
                    .map(|id| id as i32)
                    .unwrap_or(-1))
            })
        },
    )
    .map_err(|error| error.to_string())?;
    let query = Function::new(
        ctx.clone(),
        |ctx: Ctx<'_>, scope: i32, selector: String, all: bool| -> rquickjs::Result<Vec<i32>> {
            with_host(&ctx, |page| {
                let scope = (scope >= 0).then_some(scope as usize);
                Ok(page
                    .document
                    .query(scope, &selector, all)
                    .into_iter()
                    .map(|id| id as i32)
                    .collect())
            })
        },
    )
    .map_err(|error| error.to_string())?;
    let text = Function::new(
        ctx.clone(),
        |ctx: Ctx<'_>, id: u32| -> rquickjs::Result<String> {
            with_host(&ctx, |page| Ok(page.document.text_of(id as usize)))
        },
    )
    .map_err(|error| error.to_string())?;
    let set_text = Function::new(
        ctx.clone(),
        |ctx: Ctx<'_>, id: u32, value: String| -> rquickjs::Result<()> {
            with_host(&ctx, |page| {
                page.document.set_text(id as usize, &value);
                Ok(())
            })
        },
    )
    .map_err(|error| error.to_string())?;
    let set_html = Function::new(
        ctx.clone(),
        |ctx: Ctx<'_>, id: u32, value: String| -> rquickjs::Result<()> {
            with_host(&ctx, |page| {
                replace_children(&mut page.document, id as usize, &value)?;
                page.clicks.retain(|(node, _)| {
                    page.document
                        .nodes
                        .get(*node)
                        .is_some_and(|node| node.parent.is_some())
                });
                Ok(())
            })
        },
    )
    .map_err(|error| error.to_string())?;
    let create = Function::new(
        ctx.clone(),
        |ctx: Ctx<'_>, name: String| -> rquickjs::Result<i32> {
            with_host(&ctx, |page| {
                if page.document.nodes.len() >= 10_000 {
                    return Err("The document exceeds the node limit.".into());
                }
                Ok(page.document.create_element(None, &name) as i32)
            })
        },
    )
    .map_err(|error| error.to_string())?;
    let append = Function::new(
        ctx.clone(),
        |ctx: Ctx<'_>, parent: u32, child: u32| -> rquickjs::Result<()> {
            with_host(&ctx, |page| {
                page.document.append_child(parent as usize, child as usize)
            })
        },
    )
    .map_err(|error| error.to_string())?;
    let remove = Function::new(
        ctx.clone(),
        |ctx: Ctx<'_>, id: u32| -> rquickjs::Result<()> {
            with_host(&ctx, |page| {
                page.document.remove_node(id as usize);
                Ok(())
            })
        },
    )
    .map_err(|error| error.to_string())?;
    let attribute = Function::new(
        ctx.clone(),
        |ctx: Ctx<'_>, id: u32, name: String| -> rquickjs::Result<String> {
            with_host(&ctx, |page| {
                Ok(page
                    .document
                    .nodes
                    .get(id as usize)
                    .and_then(|node| node.attr(&name.to_ascii_lowercase()))
                    .unwrap_or("")
                    .to_owned())
            })
        },
    )
    .map_err(|error| error.to_string())?;
    let set_attribute = Function::new(
        ctx.clone(),
        |ctx: Ctx<'_>, id: u32, name: String, value: String| -> rquickjs::Result<bool> {
            with_host(&ctx, |page| {
                Ok(page.document.set_attribute(id as usize, &name, &value))
            })
        },
    )
    .map_err(|error| error.to_string())?;
    let remove_attribute = Function::new(
        ctx.clone(),
        |ctx: Ctx<'_>, id: u32, name: String| -> rquickjs::Result<()> {
            with_host(&ctx, |page| {
                page.document.remove_attribute(id as usize, &name);
                Ok(())
            })
        },
    )
    .map_err(|error| error.to_string())?;
    let class_list = Function::new(
        ctx.clone(),
        |ctx: Ctx<'_>, id: u32, action: String, name: String| -> rquickjs::Result<bool> {
            with_host(&ctx, |page| {
                Ok(page.document.class_list(id as usize, &action, &name))
            })
        },
    )
    .map_err(|error| error.to_string())?;
    let children = Function::new(
        ctx.clone(),
        |ctx: Ctx<'_>, id: u32| -> rquickjs::Result<Vec<i32>> {
            with_host(&ctx, |page| {
                Ok(page
                    .document
                    .element_children(id as usize)
                    .into_iter()
                    .map(|id| id as i32)
                    .collect())
            })
        },
    )
    .map_err(|error| error.to_string())?;
    let body = Function::new(ctx.clone(), |ctx: Ctx<'_>| -> rquickjs::Result<i32> {
        with_host(&ctx, |page| Ok(page.document.ensure_body() as i32))
    })
    .map_err(|error| error.to_string())?;
    let root = Function::new(ctx.clone(), |ctx: Ctx<'_>| -> rquickjs::Result<i32> {
        with_host(&ctx, |page| {
            Ok(page
                .document
                .tree_order()
                .find(|id| page.document.nodes[*id].name() == "html")
                .unwrap_or(0) as i32)
        })
    })
    .map_err(|error| error.to_string())?;
    let title = Function::new(ctx.clone(), |ctx: Ctx<'_>| -> rquickjs::Result<String> {
        with_host(&ctx, |page| Ok(page.document.title().to_owned()))
    })
    .map_err(|error| error.to_string())?;
    let set_title = Function::new(
        ctx.clone(),
        |ctx: Ctx<'_>, value: String| -> rquickjs::Result<()> {
            with_host(&ctx, |page| {
                page.document.set_title(&value);
                Ok(())
            })
        },
    )
    .map_err(|error| error.to_string())?;
    let style = Function::new(
        ctx.clone(),
        |ctx: Ctx<'_>, id: u32, name: String| -> rquickjs::Result<String> {
            with_host(&ctx, |page| {
                Ok(style_value(
                    page.document
                        .nodes
                        .get(id as usize)
                        .and_then(|node| node.attr("style"))
                        .unwrap_or(""),
                    &name,
                ))
            })
        },
    )
    .map_err(|error| error.to_string())?;
    let set_style = Function::new(
        ctx.clone(),
        |ctx: Ctx<'_>, id: u32, name: String, value: String| -> rquickjs::Result<()> {
            with_host(&ctx, |page| {
                let current = page
                    .document
                    .nodes
                    .get(id as usize)
                    .and_then(|node| node.attr("style"))
                    .unwrap_or("")
                    .to_owned();
                let next = if name == "cssText" {
                    value
                } else {
                    set_style_value(&current, &name, &value)
                };
                if !page.document.set_attribute(id as usize, "style", &next) {
                    return Err("Could not update that style.".into());
                }
                Ok(())
            })
        },
    )
    .map_err(|error| error.to_string())?;
    let listen = Function::new(ctx.clone(), listen).map_err(|error| error.to_string())?;
    let tag = Function::new(
        ctx.clone(),
        |ctx: Ctx<'_>, id: u32| -> rquickjs::Result<String> {
            with_host(&ctx, |page| {
                Ok(page
                    .document
                    .nodes
                    .get(id as usize)
                    .map(|node| node.name().to_ascii_uppercase())
                    .unwrap_or_default())
            })
        },
    )
    .map_err(|error| error.to_string())?;
    let later = Function::new(ctx.clone(), later).map_err(|error| error.to_string())?;
    host.set("byId", by_id).map_err(|error| error.to_string())?;
    host.set("query", query)
        .map_err(|error| error.to_string())?;
    host.set("text", text).map_err(|error| error.to_string())?;
    host.set("setText", set_text)
        .map_err(|error| error.to_string())?;
    host.set("setHtml", set_html)
        .map_err(|error| error.to_string())?;
    host.set("create", create)
        .map_err(|error| error.to_string())?;
    host.set("append", append)
        .map_err(|error| error.to_string())?;
    host.set("remove", remove)
        .map_err(|error| error.to_string())?;
    host.set("attribute", attribute)
        .map_err(|error| error.to_string())?;
    host.set("setAttribute", set_attribute)
        .map_err(|error| error.to_string())?;
    host.set("removeAttribute", remove_attribute)
        .map_err(|error| error.to_string())?;
    host.set("classList", class_list)
        .map_err(|error| error.to_string())?;
    host.set("children", children)
        .map_err(|error| error.to_string())?;
    host.set("body", body).map_err(|error| error.to_string())?;
    host.set("root", root).map_err(|error| error.to_string())?;
    host.set("title", title)
        .map_err(|error| error.to_string())?;
    host.set("setTitle", set_title)
        .map_err(|error| error.to_string())?;
    host.set("style", style)
        .map_err(|error| error.to_string())?;
    host.set("setStyle", set_style)
        .map_err(|error| error.to_string())?;
    host.set("listen", listen)
        .map_err(|error| error.to_string())?;
    host.set("tag", tag).map_err(|error| error.to_string())?;
    host.set("later", later)
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn listen<'js>(ctx: Ctx<'js>, id: u32, callback: Function<'js>) -> rquickjs::Result<()> {
    let saved = Persistent::save(&ctx, callback);
    with_host(&ctx, |page| {
        let id = id as usize;
        if page.document.nodes.get(id).is_none() {
            return Err("The element no longer exists.".into());
        }
        if page.clicks.len() < 256 {
            page.document.clickable.insert(id);
            page.clicks.push((id, saved));
        }
        Ok(())
    })
}

fn later<'js>(ctx: Ctx<'js>, callback: Function<'js>) -> rquickjs::Result<()> {
    let saved = Persistent::save(&ctx, callback);
    with_host(&ctx, |page| {
        if page.tasks.len() < MAX_TASKS {
            page.tasks.push(saved);
        }
        Ok(())
    })
}

fn drain_tasks(ctx: &Ctx<'_>, page: &mut Page) {
    for _ in 0..MAX_TASKS {
        if page.tasks.is_empty() {
            break;
        }
        let task = page.tasks.remove(0);
        let Ok(function) = task.restore(ctx) else {
            continue;
        };
        if let Err(error) = function.call::<_, rquickjs::Value>(()) {
            page.errors.push(explain(ctx, error));
        }
    }
}

fn ancestor_chain(document: &Document, node: NodeId) -> Vec<NodeId> {
    let mut chain = Vec::new();
    let mut current = Some(node);
    while let Some(id) = current {
        if document.nodes.get(id).is_none() {
            break;
        }
        chain.push(id);
        current = document.nodes[id].parent;
        if chain.len() == 128 {
            break;
        }
    }
    chain
}

fn explain(ctx: &Ctx<'_>, error: rquickjs::Error) -> String {
    let caught = ctx.catch();
    let message = caught
        .as_string()
        .and_then(|text| text.to_string().ok())
        .unwrap_or_else(|| error.to_string());
    if message.is_empty() {
        "A script failed.".into()
    } else {
        message
    }
}

struct Prepared {
    name: String,
    code: String,
    module: bool,
}

fn prepare_script(page: &Page, script: &crate::dom::ScriptBlock) -> Option<Prepared> {
    let name = script
        .src
        .clone()
        .unwrap_or_else(|| format!("inline:{}", script.id));
    let source = if let Some(src) = &script.src {
        page.sources.get(src)?.as_str()
    } else if script.code.len() <= MAX_SOURCE {
        script.code.as_str()
    } else {
        return None;
    };
    if source.len() > MAX_SOURCE {
        return None;
    }
    let stripped = if script.typescript || looks_like_typescript(source) {
        strip_typescript(source)
    } else {
        source.to_owned()
    };
    let module = script.module || has_module_syntax(&stripped);
    let code = if module {
        wrap_module(&stripped, &name)
    } else {
        stripped
    };
    Some(Prepared { name, code, module })
}

pub fn module_imports(source: &str) -> Vec<String> {
    let source = if looks_like_typescript(source) {
        strip_typescript(source)
    } else {
        source.to_owned()
    };
    import_specifiers(&source)
}

fn looks_like_typescript(source: &str) -> bool {
    source.contains("interface ")
        || source.contains("type ")
        || source.contains(": string")
        || source.contains(": number")
        || source.contains(": boolean")
        || source.contains(" as ")
}

const BOOTSTRAP: &str = r#"
(function () {
  function wrap(id) {
    if (!(id >= 0)) return null;
    const node = Object.create(proto);
    node.__id = id;
    return node;
  }
  const proto = {
    get textContent() { return __host.text(this.__id); },
    set textContent(value) { __host.setText(this.__id, String(value)); },
    get innerText() { return this.textContent; },
    set innerText(value) { this.textContent = value; },
    get innerHTML() { return this.textContent; },
    set innerHTML(value) { __host.setHtml(this.__id, String(value)); },
    get id() { return __host.attribute(this.__id, "id"); },
    set id(value) { __host.setAttribute(this.__id, "id", String(value)); },
    get className() { return __host.attribute(this.__id, "class"); },
    set className(value) { __host.setAttribute(this.__id, "class", String(value)); },
    get tagName() { return __host.tag(this.__id); },
    get clientWidth() { return innerWidth; },
    get clientHeight() { return innerHeight; },
    get classList() {
      const id = this.__id;
      return {
        add() { for (const name of arguments) __host.classList(id, "add", String(name)); },
        remove() { for (const name of arguments) __host.classList(id, "remove", String(name)); },
        toggle(name) { return __host.classList(id, "toggle", String(name)); },
        contains(name) { return __host.classList(id, "contains", String(name)); }
      };
    },
    get style() {
      const id = this.__id;
      return new Proxy({}, {
        get(_, prop) { return __host.style(id, String(prop)); },
        set(_, prop, value) { __host.setStyle(id, String(prop), String(value)); return true; }
      });
    },
    get children() { return __host.children(this.__id).map(wrap); },
    setAttribute(name, value) { __host.setAttribute(this.__id, String(name), String(value)); },
    getAttribute(name) { return __host.attribute(this.__id, String(name)); },
    removeAttribute(name) { __host.removeAttribute(this.__id, String(name)); },
    appendChild(child) { __host.append(this.__id, child.__id); return child; },
    remove() { __host.remove(this.__id); },
    querySelector(selector) { const found = __host.query(this.__id, String(selector), false); return found.length ? wrap(found[0]) : null; },
    querySelectorAll(selector) { return __host.query(this.__id, String(selector), true).map(wrap); },
    addEventListener(type, fn) { if (type === "click" && typeof fn === "function") __host.listen(this.__id, fn); },
    set onclick(fn) { this.addEventListener("click", fn); }
  };
  globalThis.document = {
    getElementById(id) { return wrap(__host.byId(String(id))); },
    querySelector(selector) { const found = __host.query(-1, String(selector), false); return found.length ? wrap(found[0]) : null; },
    querySelectorAll(selector) { return __host.query(-1, String(selector), true).map(wrap); },
    createElement(name) {
      const node = wrap(__host.create(String(name)));
      node.setAttribute("data-ko4-tag", String(name).toUpperCase());
      return node;
    },
    get body() { return wrap(__host.body()); },
    get documentElement() { return wrap(__host.root()); },
    get title() { return __host.title(); },
    set title(value) { __host.setTitle(String(value)); },
    addEventListener(type, fn) { this.documentElement.addEventListener(type, fn); }
  };
  globalThis.console = { log() {}, warn() {}, error() {}, info() {}, debug() {} };
  globalThis.setTimeout = function (fn) { if (typeof fn === "function") __host.later(fn); return 0; };
  globalThis.clearTimeout = function () {};
  globalThis.requestAnimationFrame = function (fn) { if (typeof fn === "function") __host.later(fn); return 0; };
  globalThis.addEventListener = function (type, fn) { document.addEventListener(type, fn); };
  const modules = new Map();
  const factories = new Map();
  globalThis.__ko4define = function (name, factory) { factories.set(name, factory); };
  globalThis.__ko4mod = function (name) {
    if (modules.has(name)) return modules.get(name);
    const factory = factories.get(name);
    if (typeof factory !== "function") throw new Error("Cannot import " + name);
    const exports = {};
    modules.set(name, exports);
    factory(exports);
    return exports;
  };
})();
"#;

fn style_value(style: &str, name: &str) -> String {
    let name = css_name(name);
    style
        .split(';')
        .find_map(|part| {
            let (key, value) = part.split_once(':')?;
            key.trim()
                .eq_ignore_ascii_case(&name)
                .then(|| value.trim().to_owned())
        })
        .unwrap_or_default()
}

fn set_style_value(style: &str, name: &str, value: &str) -> String {
    let name = css_name(name);
    let mut kept = Vec::new();
    for part in style.split(';') {
        let Some((key, _)) = part.split_once(':') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case(&name) && !key.trim().is_empty() {
            kept.push(part.trim().to_owned());
        }
    }
    if !value.trim().is_empty() && value.len() <= 512 {
        kept.push(format!("{name}: {}", value.trim()));
    }
    kept.join("; ")
}

fn css_name(name: &str) -> String {
    match name {
        "cssText" => "css-text".into(),
        "backgroundColor" => "background-color".into(),
        "fontSize" => "font-size".into(),
        "fontWeight" => "font-weight".into(),
        "textAlign" => "text-align".into(),
        "minWidth" => "min-width".into(),
        "maxWidth" => "max-width".into(),
        "minHeight" => "min-height".into(),
        "flexDirection" => "flex-direction".into(),
        "justifyContent" => "justify-content".into(),
        "alignItems" => "align-items".into(),
        "borderRadius" => "border-radius".into(),
        "borderColor" => "border-color".into(),
        "borderWidth" => "border-width".into(),
        _ => {
            let mut out = String::new();
            for character in name.chars() {
                if character.is_ascii_uppercase() {
                    out.push('-');
                    out.extend(character.to_lowercase());
                } else {
                    out.push(character);
                }
            }
            out
        }
    }
}

fn strip_if_needed(source: &str) -> String {
    if looks_like_typescript(source) {
        strip_typescript(source)
    } else {
        source.to_owned()
    }
}

fn js_string(value: &str) -> String {
    let mut out = String::from("\"");
    for character in value.chars() {
        match character {
            '\\' | '"' => {
                out.push('\\');
                out.push(character);
            }
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

struct Scan<'a> {
    source: &'a str,
    index: usize,
}

impl<'a> Scan<'a> {
    fn rest(&self) -> &'a str {
        &self.source[self.index..]
    }

    fn skip_space(&mut self) {
        self.index += self
            .rest()
            .chars()
            .take_while(|character| character.is_whitespace())
            .map(|character| character.len_utf8())
            .sum::<usize>();
    }

    fn eat(&mut self, value: &str) -> bool {
        if self.rest().starts_with(value) {
            self.index += value.len();
            true
        } else {
            false
        }
    }

    fn identifier(&mut self) -> Option<&'a str> {
        self.skip_space();
        let bytes = self.rest().as_bytes();
        if bytes.first().is_none_or(|byte| !is_ident_start(*byte)) {
            return None;
        }
        let mut end = 1;
        while bytes.get(end).is_some_and(|byte| is_ident_continue(*byte)) {
            end += 1;
        }
        let value = &self.rest()[..end];
        self.index += end;
        Some(value)
    }
}

fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte == b'$'
}

fn is_ident_continue(byte: u8) -> bool {
    is_ident_start(byte) || byte.is_ascii_digit()
}

fn strip_typescript(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut index = 0;
    let mut out = String::with_capacity(source.len());
    let mut braces: Vec<u8> = Vec::new();
    let mut questions = 0i32;
    let mut expect_class = false;
    while index < bytes.len() {
        if copy_literal(source, &mut index, &mut out) {
            continue;
        }
        let byte = bytes[index];
        if byte == b'{' {
            let kind = if expect_class {
                expect_class = false;
                b'c'
            } else if out
                .trim_end()
                .ends_with(['=', '(', '[', ',', ':', '?', '&', '|', '{'])
                || out.trim_end().ends_with("return")
            {
                b'o'
            } else {
                b'b'
            };
            braces.push(kind);
            out.push('{');
            index += 1;
            continue;
        }
        if byte == b'}' {
            braces.pop();
            out.push('}');
            index += 1;
            continue;
        }
        if byte == b'?' && !source[index..].starts_with("?.") {
            if source[index + 1..].trim_start().starts_with(':') {
                index += 1;
                continue;
            }
            questions += 1;
            out.push('?');
            index += 1;
            continue;
        }
        if byte == b':' && questions > 0 {
            questions -= 1;
            out.push(':');
            index += 1;
            continue;
        }
        if byte == b':' && questions == 0 && braces.last().copied() != Some(b'o') {
            index += 1;
            skip_type(source, &mut index);
            continue;
        }
        if byte == b'!' && !source[index..].starts_with("!=") && !source[index..].starts_with("!==")
        {
            index += 1;
            continue;
        }
        if is_ident_start(byte) {
            let start = index;
            index += 1;
            while index < bytes.len() && is_ident_continue(bytes[index]) {
                index += 1;
            }
            let word = &source[start..index];
            match word {
                "interface" | "enum" | "namespace" => {
                    skip_statement(source, &mut index);
                    out.push(' ');
                }
                "type" if type_alias(source, index) => {
                    skip_statement(source, &mut index);
                    out.push(' ');
                }
                "declare" => {
                    skip_statement(source, &mut index);
                    out.push(' ');
                }
                "class" => {
                    expect_class = true;
                    out.push_str(word);
                }
                "public" | "private" | "protected" | "readonly" | "abstract" | "override" => {
                    out.push(' ');
                }
                "implements" => skip_until_brace(source, &mut index),
                "as" if out.ends_with(|c: char| c.is_whitespace() || c == ')') => {
                    skip_type(source, &mut index);
                }
                _ => out.push_str(word),
            }
            continue;
        }
        if byte.is_ascii() {
            out.push(byte as char);
            index += 1;
        } else {
            let character = source[index..].chars().next().unwrap_or('\u{fffd}');
            out.push(character);
            index += character.len_utf8();
        }
    }
    out
}

fn type_alias(source: &str, index: usize) -> bool {
    let mut scan = Scan { source, index };
    scan.identifier().is_some()
}

fn skip_statement(source: &str, index: &mut usize) {
    let bytes = source.as_bytes();
    let mut depth = 0i32;
    while *index < bytes.len() {
        if copy_literal(source, index, &mut String::new()) {
            continue;
        }
        match bytes[*index] {
            b'{' => depth += 1,
            b'}' if depth > 0 => depth -= 1,
            b'}' => break,
            b';' if depth == 0 => {
                *index += 1;
                break;
            }
            _ => {}
        }
        *index += 1;
        if depth == 0 && bytes.get(*index - 1) == Some(&b'}') {
            break;
        }
    }
}

fn skip_until_brace(source: &str, index: &mut usize) {
    if let Some(at) = source[*index..].find('{') {
        *index += at;
    }
}

fn skip_type(source: &str, index: &mut usize) {
    let bytes = source.as_bytes();
    let mut depth = 0i32;
    while *index < bytes.len() {
        if copy_literal(source, index, &mut String::new()) {
            continue;
        }
        let byte = bytes[*index];
        match byte {
            b'(' | b'[' | b'{' | b'<' => depth += 1,
            b')' | b']' | b'}' | b'>' if depth > 0 => depth -= 1,
            b'=' if depth == 0 && source[*index..].starts_with("=>") => break,
            b',' | b';' | b'=' | b'?' | b')' | b']' | b'}' if depth == 0 => break,
            b'&' | b'|'
                if depth == 0 && matches!(source.as_bytes().get(*index + 1), Some(b'&' | b'|')) =>
            {
                break;
            }
            _ => {}
        }
        *index += 1;
    }
}

fn copy_literal(source: &str, index: &mut usize, out: &mut String) -> bool {
    let rest = &source[*index..];
    let bytes = rest.as_bytes();
    if rest.starts_with("//") {
        let end = rest.find('\n').map_or(rest.len(), |at| at + 1);
        out.push_str(&rest[..end]);
        *index += end;
        return true;
    }
    if rest.starts_with("/*") {
        let end = rest.find("*/").map_or(rest.len(), |at| at + 2);
        out.push_str(&rest[..end]);
        *index += end;
        return true;
    }
    let quote = match bytes.first() {
        Some(b'"' | b'\'' | b'`') => bytes[0],
        _ => return false,
    };
    let mut cursor = 1;
    while cursor < bytes.len() {
        let byte = bytes[cursor];
        cursor += 1;
        if byte == b'\\' && cursor < bytes.len() {
            cursor += 1;
            continue;
        }
        if byte == quote {
            break;
        }
    }
    out.push_str(&rest[..cursor]);
    *index += cursor;
    true
}

fn has_module_syntax(source: &str) -> bool {
    !import_specifiers(source).is_empty() || export_at_boundary(source)
}

fn export_at_boundary(source: &str) -> bool {
    let bytes = source.as_bytes();
    let mut index = 0;
    let mut depth = 0i32;
    while index < bytes.len() {
        if copy_literal(source, &mut index, &mut String::new()) {
            continue;
        }
        match bytes[index] {
            b'{' | b'(' | b'[' => depth += 1,
            b'}' | b')' | b']' if depth > 0 => depth -= 1,
            _ => {}
        }
        if depth == 0 && keyword_at(source, index, "export") {
            return true;
        }
        index += 1;
    }
    false
}

fn import_specifiers(source: &str) -> Vec<String> {
    let mut found = Vec::new();
    let bytes = source.as_bytes();
    let mut index = 0;
    let mut depth = 0i32;
    while index < bytes.len() {
        if copy_literal(source, &mut index, &mut String::new()) {
            continue;
        }
        match bytes[index] {
            b'{' | b'(' | b'[' => depth += 1,
            b'}' | b')' | b']' if depth > 0 => depth -= 1,
            _ => {}
        }
        if depth == 0
            && keyword_at(source, index, "import")
            && let Some(specifier) = import_source(source, index)
        {
            found.push(specifier);
        }
        index += 1;
    }
    found
}

fn keyword_at(source: &str, index: usize, word: &str) -> bool {
    let rest = &source[index..];
    let boundary = index == 0
        || source[..index].chars().next_back().is_none_or(|character| {
            !character.is_ascii_alphanumeric() && character != '_' && character != '$'
        });
    boundary
        && rest.starts_with(word)
        && rest
            .as_bytes()
            .get(word.len())
            .is_none_or(|byte| !is_ident_continue(*byte))
}

fn import_source(source: &str, index: usize) -> Option<String> {
    let mut scan = Scan {
        source,
        index: index + "import".len(),
    };
    let end = source[index..]
        .find(';')
        .map(|at| index + at)
        .unwrap_or(source.len());
    let clause = &source[index..end];
    if clause.contains(" from ") || clause.contains("\nfrom ") {
        let marker = clause.rfind(" from ").or_else(|| clause.rfind("\nfrom "))?;
        let mut tail = Scan {
            source,
            index: index + marker + clause[marker..].find("from").unwrap_or(0) + 4,
        };
        tail.skip_space();
        return quoted(&mut tail);
    }
    scan.skip_space();
    quoted(&mut scan)
}

fn quoted(scan: &mut Scan<'_>) -> Option<String> {
    let quote = scan.rest().chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    scan.index += quote.len_utf8();
    let start = scan.index;
    while scan.index < scan.source.len() {
        let character = scan.rest().chars().next()?;
        if character == '\\' {
            scan.index += character.len_utf8();
            scan.index += scan
                .rest()
                .chars()
                .next()
                .map(|c| c.len_utf8())
                .unwrap_or(0);
            continue;
        }
        if character == quote {
            let value = scan.source[start..scan.index].to_owned();
            return (!value.is_empty() && value.len() <= 1024).then_some(value);
        }
        scan.index += character.len_utf8();
    }
    None
}

fn wrap_module(source: &str, name: &str) -> String {
    let mut out = String::new();
    let bytes = source.as_bytes();
    let mut index = 0;
    let mut depth = 0i32;
    while index < bytes.len() {
        if copy_literal(source, &mut index, &mut out) {
            continue;
        }
        match bytes[index] {
            b'{' | b'(' | b'[' => depth += 1,
            b'}' | b')' | b']' if depth > 0 => depth -= 1,
            _ => {}
        }
        if depth == 0
            && keyword_at(source, index, "import")
            && let Some(end) = source[index..].find(';')
        {
            out.push_str(&rewrite_import(&source[index..index + end], name));
            index += end + 1;
            continue;
        }
        if depth == 0 && keyword_at(source, index, "export") {
            let rewritten = rewrite_export(&source[index..]);
            out.push_str(&rewritten.code);
            index += rewritten.consumed;
            continue;
        }
        if bytes[index].is_ascii() {
            out.push(bytes[index] as char);
            index += 1;
        } else {
            let character = source[index..].chars().next().unwrap_or('\u{fffd}');
            out.push(character);
            index += character.len_utf8().max(1);
        }
    }
    format!(
        "__ko4define({}, function(exports) {{\n{out}\nreturn exports;\n}});",
        js_string(name)
    )
}

struct Rewritten {
    code: String,
    consumed: usize,
}

fn rewrite_import(statement: &str, importer: &str) -> String {
    let Some(specifier) = import_source(statement, 0) else {
        return String::new();
    };
    let _ = importer;
    let call = format!("__ko4mod({})", js_string(&specifier));
    if statement.trim() == format!("import \"{specifier}\"")
        || statement.trim() == format!("import '{specifier}'")
    {
        return format!("{call};");
    }
    let before = statement.split(" from ").next().unwrap_or(statement);
    let bindings = before.trim_start().trim_start_matches("import").trim();
    if let Some(name) = bindings.strip_prefix("* as ") {
        return format!("const {name} = {call};");
    }
    if bindings.starts_with('{') {
        return format!("const {bindings} = {call};");
    }
    if let Some((default, named)) = bindings.split_once(',') {
        return format!("const {default} = {call}.default; const {named} = {call};");
    }
    format!("const {bindings} = {call}.default;")
}

fn rewrite_export(source: &str) -> Rewritten {
    let mut scan = Scan {
        source,
        index: "export".len(),
    };
    scan.skip_space();
    if scan.eat("default") {
        let rest = scan.rest();
        let consumed = source.len().min(scan.index + statement_length(rest));
        return Rewritten {
            code: format!("exports.default = {}", &source[scan.index..consumed]),
            consumed,
        };
    }
    if scan.rest().starts_with("function")
        || scan.rest().starts_with("class")
        || scan.rest().starts_with("const")
        || scan.rest().starts_with("let")
        || scan.rest().starts_with("var")
    {
        let name = declaration_name(scan.rest()).unwrap_or("").to_owned();
        let consumed = statement_length(source);
        let mut code = source[..consumed].replacen("export", "", 1);
        if !name.is_empty() {
            code.push_str(&format!("\nexports[{}] = {name};", js_string(&name)));
        }
        return Rewritten { code, consumed };
    }
    if scan.eat("{") {
        let body = scan.rest().split('}').next().unwrap_or("");
        let mut code = String::new();
        for part in body.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let (local, exported) = part
                .split_once(" as ")
                .map(|(local, exported)| (local.trim(), exported.trim()))
                .unwrap_or((part, part));
            code.push_str(&format!("exports[{}] = {local};\n", js_string(exported)));
        }
        let consumed = source[scan.index + body.len()..]
            .find(';')
            .map(|at| scan.index + body.len() + at + 1)
            .unwrap_or(source.len());
        return Rewritten { code, consumed };
    }
    Rewritten {
        code: String::new(),
        consumed: statement_length(source),
    }
}

fn declaration_name(source: &str) -> Option<&str> {
    let mut scan = Scan { source, index: 0 };
    scan.identifier()?;
    scan.identifier()
}

fn statement_length(source: &str) -> usize {
    let bytes = source.as_bytes();
    let mut index = 0;
    let mut depth = 0i32;
    while index < bytes.len() {
        if copy_literal(source, &mut index, &mut String::new()) {
            continue;
        }
        match bytes[index] {
            b'{' | b'(' => depth += 1,
            b'}' | b')' if depth > 0 => depth -= 1,
            b';' if depth == 0 => return index + 1,
            _ => {}
        }
        index += 1;
        if depth == 0
            && bytes.get(index - 1) == Some(&b'}')
            && !source[index..].trim_start().starts_with('.')
        {
            return index;
        }
    }
    source.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{layout, parse_html};
    use display_list::{TextMetrics, TextRange, TextStyle};

    struct Capture(String);
    impl display_list::TextMeasurer for Capture {
        fn measure(
            &mut self,
            text: &str,
            style: TextStyle,
            _width: f32,
            _links: &[TextRange],
        ) -> Result<TextMetrics, String> {
            self.0.push_str(text);
            Ok(TextMetrics {
                layout: 0,
                width: text.chars().count() as f32 * 8.0,
                height: style.size,
                links: Vec::new(),
            })
        }
    }

    #[test]
    fn typescript_builds_a_styled_element_and_a_click_updates_it() {
        let mut session = ScriptSession::open(
            r##"
            <body style="margin:0">
              <div id="app"></div>
              <button id="go">Go</button>
              <script type="text/typescript">
                interface Greeting { text: string }
                const greeting: Greeting = { text: "Привет" };
                const app = document.getElementById("app");
                app!.textContent = greeting.text;
                app.style.background = "#112233";
                app.style.padding = "4px";
                document.getElementById("go").addEventListener("click", () => {
                  app.textContent = "Clicked";
                });
              </script>
            </body>
            "##,
        )
        .unwrap();
        session
            .run(Viewport {
                width: 640.0,
                height: 480.0,
            })
            .unwrap();
        let mut capture = Capture(String::new());
        let page = layout(&session.document(), 640.0, &mut capture, |_| None, None).unwrap();
        assert!(
            capture.0.contains("Привет"),
            "text={:?} script={:?}",
            capture.0,
            session.failure()
        );
        assert!(page.display_list.commands.iter().any(|command| {
            matches!(command, display_list::Command::Fill { color, .. } if *color == display_list::Color::rgb(0x112233))
        }));
        let button = session.document().element_by_id("go").unwrap();
        session
            .click(
                button as u32,
                Viewport {
                    width: 640.0,
                    height: 480.0,
                },
            )
            .unwrap();
        capture.0.clear();
        layout(&session.document(), 640.0, &mut capture, |_| None, None).unwrap();
        assert!(capture.0.contains("Clicked"), "{}", capture.0);
        let _ = parse_html("<p></p>");
    }

    #[test]
    fn strips_types_without_touching_object_literals() {
        let source = strip_typescript(
            "const box: { label: string } = { label: \"ok\", count: 1 }; function ping(value: string): string { return value; }",
        );
        assert!(source.contains("{ label: \"ok\", count: 1 }"), "{source}");
        assert!(!source.contains(": string"), "{source}");
        assert!(source.contains("function ping(value)"), "{source}");
    }
}
