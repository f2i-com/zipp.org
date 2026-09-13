import {check, ComputeError} from '../graph.mjs';

/** Fragment-shader compute using RGBA32F textures. One scalar occupies the red channel. */
export class WebGL2Backend {
  static async create() {
    const canvas=typeof OffscreenCanvas!=='undefined'?new OffscreenCanvas(1,1):globalThis.document?.createElement('canvas');
    check(canvas, 'UNAVAILABLE', 'No canvas implementation is available');
    const gl=canvas.getContext('webgl2',{antialias:false,depth:false,stencil:false,preserveDrawingBuffer:false});
    check(gl,'UNAVAILABLE','WebGL2 is unavailable');
    if(!gl.getExtension('EXT_color_buffer_float')) {
      gl.getExtension('WEBGL_lose_context')?.loseContext();
      throw new ComputeError('UNAVAILABLE','WebGL2 floating-point render targets are unavailable');
    }
    return new WebGL2Backend(gl,canvas);
  }
  constructor(gl,canvas) {
    this.name='webgl2';this.description='WebGL2 fragment shaders on RGBA32F textures';
    this.gl=gl;this.canvas=canvas;this.programs=new Map();this.lost=false;
    this.maxTexture=gl.getParameter(gl.MAX_TEXTURE_SIZE);
    const viewport=gl.getParameter(gl.MAX_VIEWPORT_DIMS);
    this.maxWidth=Math.min(this.maxTexture,viewport[0]);this.maxHeight=Math.min(this.maxTexture,viewport[1]);
    this.framebuffer=gl.createFramebuffer();this.vao=gl.createVertexArray();
    canvas.addEventListener?.('webglcontextlost',()=>{this.lost=true;});
    gl.disable(gl.DEPTH_TEST);gl.disable(gl.BLEND);gl.disable(gl.DITHER);gl.bindVertexArray(this.vao);
  }
  live(){check(!this.lost&&!this.gl.isContextLost(),'DEVICE_LOST','WebGL context is lost');}
  async begin(){this.live();}
  alloc(size,data) {
    this.live();const gl=this.gl;
    const width=Math.min(size,1024,this.maxWidth),height=Math.ceil(size/width);
    check(height<=this.maxHeight,'LIMIT','Tensor exceeds WebGL texture/viewport limits');
    const texture=gl.createTexture();check(texture,'GPU','WebGL texture allocation failed');
    gl.bindTexture(gl.TEXTURE_2D,texture);
    gl.texParameteri(gl.TEXTURE_2D,gl.TEXTURE_MIN_FILTER,gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D,gl.TEXTURE_MAG_FILTER,gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D,gl.TEXTURE_WRAP_S,gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D,gl.TEXTURE_WRAP_T,gl.CLAMP_TO_EDGE);
    let rgba=null;
    if(data) {rgba=new Float32Array(width*height*4);for(let i=0;i<size;i++)rgba[4*i]=data[i];}
    gl.texImage2D(gl.TEXTURE_2D,0,gl.RGBA32F,width,height,0,gl.RGBA,gl.FLOAT,rgba);
    const error=gl.getError();
    if(error!==gl.NO_ERROR){gl.deleteTexture(texture);throw new ComputeError('GPU',`Texture allocation error ${error}`);}
    return {texture,width,height,size};
  }
  shader(type,code) {
    const gl=this.gl,s=gl.createShader(type);check(s,'GPU','Shader allocation failed');
    gl.shaderSource(s,code);gl.compileShader(s);
    if(!gl.getShaderParameter(s,gl.COMPILE_STATUS)) {
      const reason=gl.getShaderInfoLog(s);gl.deleteShader(s);throw new ComputeError('SHADER',reason||'Shader compilation failed');
    }
    return s;
  }
  program(fragment) {
    if(this.programs.has(fragment))return this.programs.get(fragment);
    const gl=this.gl;let vs,fs,p;
    try {
      vs=this.shader(gl.VERTEX_SHADER,`#version 300 es
      const vec2 positions[3]=vec2[3](vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.));
      void main(){gl_Position=vec4(positions[gl_VertexID],0.,1.);}`);
      fs=this.shader(gl.FRAGMENT_SHADER,fragment);p=gl.createProgram();check(p,'GPU','Program allocation failed');
      gl.attachShader(p,vs);gl.attachShader(p,fs);gl.linkProgram(p);
      check(gl.getProgramParameter(p,gl.LINK_STATUS),'SHADER',gl.getProgramInfoLog(p)||'Shader link failed');
      if(this.programs.size>=128){const key=this.programs.keys().next().value;gl.deleteProgram(this.programs.get(key));this.programs.delete(key);}
      this.programs.set(fragment,p);return p;
    } catch(error) {if(p)gl.deleteProgram(p);throw error;}
    finally {if(vs)gl.deleteShader(vs);if(fs)gl.deleteShader(fs);}
  }
  attach(h) {
    const gl=this.gl;gl.bindFramebuffer(gl.FRAMEBUFFER,this.framebuffer);
    gl.framebufferTexture2D(gl.FRAMEBUFFER,gl.COLOR_ATTACHMENT0,gl.TEXTURE_2D,h.texture,0);
    check(gl.checkFramebufferStatus(gl.FRAMEBUFFER)===gl.FRAMEBUFFER_COMPLETE,'GPU','Float framebuffer is incomplete');
  }
  dispatch(out,inputs,body) {
    this.live();const gl=this.gl;
    const declarations=inputs.map((h,i)=>`uniform highp sampler2D tex${i};
      float ${i===0?'A':'B'}(int j){return texelFetch(tex${i},ivec2(j%${h.width},j/${h.width}),0).r;}`).join('\n');
    const code=`#version 300 es
      precision highp float; precision highp int;
      ${declarations}
      out vec4 resultColor;
      void main(){int i=int(gl_FragCoord.y)*${out.width}+int(gl_FragCoord.x);
        if(i>=${out.size}){resultColor=vec4(0.);return;}
        float value=0.; ${body} resultColor=vec4(value,0.,0.,0.);}`;
    const program=this.program(code);gl.useProgram(program);this.attach(out);gl.viewport(0,0,out.width,out.height);
    inputs.forEach((h,i)=>{gl.activeTexture(gl.TEXTURE0+i);gl.bindTexture(gl.TEXTURE_2D,h.texture);gl.uniform1i(gl.getUniformLocation(program,`tex${i}`),i);});
    gl.bindVertexArray(this.vao);gl.drawArrays(gl.TRIANGLES,0,3);
    const error=gl.getError();check(error===gl.NO_ERROR,'GPU',`WebGL dispatch error ${error}`);
  }
  async run(n,refs) {
    if(n.op==='input')return this.alloc(n.size,n.data);
    const out=this.alloc(n.size);
    try {
      switch(n.op) {
        case 'full':this.dispatch(out,[],`value=${glFloat(n.value)};`);break;
        case 'add':case 'sub':case 'mul':
          this.dispatch(out,refs,`value=A(${n.aScalar?'0':'i'})${{add:'+',sub:'-',mul:'*'}[n.op]}B(${n.bScalar?'0':'i'});`);break;
        case 'relu':this.dispatch(out,refs,'value=max(A(i),0.);');break;
        case 'matmul':this.dispatch(out,refs,`int row=i/${n.n};int col=i%${n.n};
          for(int k=0;k<${n.k};k++){value+=A(row*${n.k}+k)*B(k*${n.n}+col);}`);break;
        case 'sum': {
          let input=refs[0];const scratch=[];
          try {
            while(input.size>1){const length=Math.ceil(input.size/2),next=length===1?out:this.alloc(length);
              if(next!==out)scratch.push(next);
              this.dispatch(next,[input],`int j=i*2;value=A(j);if(j+1<${input.size})value+=A(j+1);`);input=next;
            }
            if(refs[0].size===1)this.dispatch(out,refs,'value=A(0);');
          } finally {for(const h of scratch)this.free(h);}break;
        }
        case 'life': {
          const [h,w]=n.shape;
          this.dispatch(out,refs,`int x=i%${w};int y=i/${w};int count=0;
            for(int dy=-1;dy<=1;dy++)for(int dx=-1;dx<=1;dx++)if(dx!=0||dy!=0){
              int xx=(x+dx+${w})%${w};int yy=(y+dy+${h})%${h};count+=A(yy*${w}+xx)>0.5?1:0;}
            value=count==3||(A(i)>0.5&&count==2)?1.:0.;`);break;
        }
      }
      return out;
    }catch(error){this.free(out);throw error;}
  }
  async read(h) {
    this.live();const gl=this.gl;this.attach(h);
    const rgba=new Float32Array(h.width*h.height*4);
    // readPixels is synchronous in this fallback. The API remains awaitable, not non-blocking.
    gl.readPixels(0,0,h.width,h.height,gl.RGBA,gl.FLOAT,rgba);
    check(gl.getError()===gl.NO_ERROR,'GPU','WebGL float readback failed');
    const result=new Float32Array(h.size);for(let i=0;i<h.size;i++)result[i]=rgba[i*4];return result;
  }
  free(h){this.gl.deleteTexture(h.texture);}
  async finish(){this.live();}
  dispose(){const gl=this.gl;for(const p of this.programs.values())gl.deleteProgram(p);this.programs.clear();gl.deleteFramebuffer(this.framebuffer);gl.deleteVertexArray(this.vao);gl.getExtension('WEBGL_lose_context')?.loseContext();}
}
function glFloat(value){const s=String(value);return s.includes('.')||s.includes('e')?s:`${s}.0`;}
