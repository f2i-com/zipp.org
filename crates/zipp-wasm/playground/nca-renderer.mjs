// Draw real tensor snapshots with one float texture and a WebGL2 fragment shader.
export function createStateRenderer(canvas) {
  const gl = canvas.getContext('webgl2', { powerPreference: 'high-performance', failIfMajorPerformanceCaveat: true, antialias: false, depth: false });
  if (!gl) throw Error('Hardware WebGL2 unavailable. Enable browser graphics acceleration and restart the browser.');
  const debug = gl.getExtension('WEBGL_debug_renderer_info');
  const description = String(gl.getParameter(debug ? debug.UNMASKED_RENDERER_WEBGL : gl.RENDERER));
  if (/swiftshader|llvmpipe|softpipe|software|microsoft basic render/i.test(description)) throw Error(`Browser returned a software renderer: ${description}`);
  const shader = (type, source) => {
    const s = gl.createShader(type); gl.shaderSource(s, source); gl.compileShader(s);
    if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw Error(gl.getShaderInfoLog(s));
    return s;
  };
  const vs = shader(gl.VERTEX_SHADER, `#version 300 es
  const vec2 p[3]=vec2[3](vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.));
  void main(){gl_Position=vec4(p[gl_VertexID],0.,1.);}`);
  const fs = shader(gl.FRAGMENT_SHADER, `#version 300 es
  precision highp float; precision highp int;
  uniform sampler2D state; uniform vec2 resolution; uniform ivec2 dims;
  uniform float scale; uniform int activeCell; uniform int ring; uniform int hasData;
  out vec4 color;
  vec3 valueColor(float v){float a=tanh(abs(v)/max(scale,.001)*1.8);return mix(vec3(.06,.105,.14),v<0.?vec3(.35,.5,.98):vec3(.34,.94,.73),a);}
  void main(){
    vec2 uv=vec2(gl_FragCoord.x/resolution.x,1.-gl_FragCoord.y/resolution.y);
    vec3 c=vec3(.035,.064,.083);
    float gridline=step(.97,fract(uv.x*40.))+step(.97,fract(uv.y*20.)); c+=gridline*.009;
    if(hasData==1){
      float left=ring==1?.36:.045;
      vec2 q=(uv-vec2(left,.09))/vec2(.95-left,.81);
      if(all(greaterThanEqual(q,vec2(0.)))&&all(lessThan(q,vec2(1.)))){
        ivec2 xy=ivec2(q*vec2(dims));float v=texelFetch(state,xy,0).r;c=valueColor(v);
        vec2 cell=fract(q*vec2(dims));
        if(cell.y<.025 || cell.x<.06)c*=.5;
        if(ring==1&&xy.y==activeCell&&(cell.y<.08||cell.y>.92))c=vec3(.85,1.,.91);
      }
      if(ring==1){
        vec2 p=(uv-vec2(.18,.49))*vec2(resolution.x/resolution.y,1.);
        float r=length(p);if(abs(r-.20)<.002)c=vec3(.17,.28,.32);
        for(int i=0;i<8;i++){
          float a=float(i)*6.2831853/8.-1.5707963;vec2 center=vec2(cos(a),sin(a))*.20;
          float d=length(p-center);float rms=0.;
          for(int j=0;j<128;j++){if(j<dims.x){float v=texelFetch(state,ivec2(j,i),0).r;rms+=v*v;}}
          rms=sqrt(rms/float(dims.x));
          if(d<.032)c=mix(vec3(.1,.18,.23),vec3(.35,.92,.75),tanh(rms/max(scale,.001)));
          if(i==activeCell&&d>.034&&d<.041)c=vec3(.91,.98,.73);
        }
      }
    }
    color=vec4(c,1.);
  }`);
  const program = gl.createProgram(); gl.attachShader(program, vs); gl.attachShader(program, fs); gl.linkProgram(program);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) throw Error(gl.getProgramInfoLog(program));
  gl.deleteShader(vs); gl.deleteShader(fs);
  const vao = gl.createVertexArray(); gl.bindVertexArray(vao);
  const texture = gl.createTexture(); gl.bindTexture(gl.TEXTURE_2D, texture);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST); gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE); gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
  gl.useProgram(program);
  const uniform = name => gl.getUniformLocation(program, name);
  let lost = false;
  canvas.addEventListener('webglcontextlost', event => { event.preventDefault(); lost = true; });
  function draw(frame, memory = true) {
    if (lost || gl.isContextLost()) throw Error('WebGL context lost. Reload to restore visualization; the native run continues.');
    const data = frame?.grid || { width: 1, height: 1, values: [0] };
    const values = new Float32Array(data.values);
    const scale = Math.max(.001, Math.sqrt(values.reduce((sum, v) => sum + v * v, 0) / values.length));
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.R32F, data.width, data.height, 0, gl.RED, gl.FLOAT, values);
    gl.uniform1i(uniform('state'), 0); gl.uniform2i(uniform('dims'), data.width, data.height);
    gl.uniform2f(uniform('resolution'), canvas.width, canvas.height); gl.uniform1f(uniform('scale'), scale);
    gl.uniform1i(uniform('activeCell'), frame?.cell ?? -1); gl.uniform1i(uniform('ring'), memory ? 1 : 0);
    gl.uniform1i(uniform('hasData'), frame ? 1 : 0); gl.viewport(0, 0, canvas.width, canvas.height);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
  }
  draw(null);
  return { draw, description };
}
