setTimeout(() => {
  document.querySelector('#app').innerHTML = '<main id="ready"><h1>Rendered guide</h1><p>JS content with <a href="/reference">a reference</a> and <code>cargo test</code>.</p><pre><code>let x = 1;</code></pre><table><tr><th>Name</th><th>Value</th></tr><tr><td>A</td><td>1</td></tr></table><p class="css-hidden">Hidden CSS clutter</p></main>';
}, 150);
