var u=new Set(["__proto__","constructor","prototype"]);function c(e,a){if(!(!e||typeof e!="object"||u.has(a)))return Object.prototype.hasOwnProperty.call(e,a)?e[a]:void 0}function g(){if(typeof customElements>"u"||customElements.get("softn-datagrid"))return;class e extends HTMLElement{static get observedAttributes(){return ["rows","columns","searchable"]}_rows=[];_sortKey="";_sortDir=1;_query="";root;constructor(){super(),this.root=this.attachShadow({mode:"open"});}get rows(){return this._rows}set rows(o){this._rows=Array.isArray(o)?o.slice(0,5e3).filter(s=>!!(s&&typeof s=="object"&&!Array.isArray(s))):[],this.render();}connectedCallback(){this.syncRowsFromAttr(),this.render();}attributeChangedCallback(o){o==="rows"&&this.syncRowsFromAttr(),this.render();}syncRowsFromAttr(){let o=this.getAttribute("rows");if(o)try{let s=JSON.parse(o);Array.isArray(s)&&(this._rows=s.slice(0,5e3).filter(i=>!!(i&&typeof i=="object"&&!Array.isArray(i))));}catch{}}render(){let o=this.getAttribute("columns"),s=(o?o.split(",").map(t=>t.trim()).filter(Boolean):this._rows.length>0?Object.keys(this._rows[0]).filter(t=>!t.startsWith("_")):[]).filter(t=>!u.has(t)).slice(0,64),i=this._query.toLowerCase(),d=i?this._rows.filter(t=>s.some(r=>String(c(t,r)??"").toLowerCase().includes(i))):[...this._rows];if(this._sortKey&&!u.has(this._sortKey)){let t=this._sortKey,r=this._sortDir;d.sort((n,_)=>{let l=c(n,t),h=c(_,t);return typeof l=="number"&&typeof h=="number"?(l-h)*r:String(l??"").localeCompare(String(h??""))*r});}let b=this.hasAttribute("searchable");this.root.innerHTML=`
        <style>
          :host { display: block; font-family: inherit; }
          .toolbar { margin-bottom: 0.5rem; }
          input { padding: 0.4rem 0.65rem; border: 1px solid var(--softn-border, #cbd5e1); border-radius: 6px; width: 100%; max-width: 280px; background: inherit; color: inherit; }
          table { width: 100%; border-collapse: collapse; font-size: 0.875rem; }
          th, td { text-align: left; padding: 0.55rem 0.75rem; border-bottom: 1px solid var(--softn-border, #e2e8f0); }
          th { cursor: pointer; user-select: none; font-weight: 600; color: var(--softn-muted, #64748b); }
          tr:hover td { background: rgba(148, 163, 184, 0.08); }
        </style>
        ${b?`<div class="toolbar"><input type="search" placeholder="Search..." value="${y(this._query)}"></div>`:""}
        <table>
          <thead>
            <tr>${s.map(t=>`<th data-col="${y(t)}">${p(t)}${this._sortKey===t?this._sortDir===1?" \u25B2":" \u25BC":""}</th>`).join("")}</tr>
          </thead>
          <tbody>
            ${d.map((t,r)=>`<tr data-idx="${r}">${s.map(n=>`<td>${p(String(c(t,n)??""))}</td>`).join("")}</tr>`).join("")}
          </tbody>
        </table>
      `;let f=this.root.querySelector("input");f?.addEventListener("input",()=>{this._query=f.value,this.render();}),this.root.querySelectorAll("th[data-col]").forEach(t=>{t.addEventListener("click",()=>{let r=t.getAttribute("data-col")??"";this._sortKey===r?this._sortDir=this._sortDir===1?-1:1:(this._sortKey=r,this._sortDir=1),this.render();});}),this.root.querySelectorAll("tr[data-idx]").forEach(t=>{t.addEventListener("click",()=>{let r=Number(t.getAttribute("data-idx")),n=d[r];n&&this.dispatchEvent(new CustomEvent("row-click",{detail:n,bubbles:true}));});});}}customElements.define("softn-datagrid",e);}function p(e){return e.replace(/&/g,"&amp;").replace(/</g,"&lt;").replace(/>/g,"&gt;")}function y(e){return p(e).replace(/"/g,"&quot;")}export{g as defineSoftnDataGrid};//# sourceMappingURL=datagrid-YEZAAKWU.js.map
//# sourceMappingURL=datagrid-YEZAAKWU.js.map