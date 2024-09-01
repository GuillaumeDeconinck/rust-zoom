use crate::pages::Home::*;
use leptos::*;
use leptos_meta::*;
use leptos_router::*;

#[component]
pub fn App() -> impl IntoView {
    let formatter = |text| format!("{text} - VideoCall.rs");
    provide_meta_context();

    view! {
        <Html lang="en"/>
        <Stylesheet id="leptos" href="/pkg/leptos_website.css"/>
        <Title formatter/>
        <Meta
            name="description"
            content="Leptos is a cutting-edge Rust web framework designed for building fast, reliable, web applications."
        />
        <Router>
            <Routes>
                <Route path="" view=Home ssr=SsrMode::Async/>
            </Routes>
        </Router>
        <!-- Matomo -->
        <script>
            "var _paq = window._paq = window._paq || [];
            _paq.push([\"setDocumentTitle\", document.domain + \"/\" + document.title]);
            _paq.push([\"setCookieDomain\", \"*.videocall.rs\"]);
            _paq.push(['trackPageView']);
            _paq.push(['enableLinkTracking']);
            (function() {
                var u=\"//matomo.videocall.rs/\";
                _paq.push(['setTrackerUrl', u+'matomo.php']);
                _paq.push(['setSiteId', '1']);
                var d=document, g=d.createElement('script'), s=d.getElementsByTagName('script')[0];
                g.async=true; g.src=u+'matomo.js'; s.parentNode.insertBefore(g,s);
            })();"
        </script>
        <!-- End Matomo Code -->
    }
}