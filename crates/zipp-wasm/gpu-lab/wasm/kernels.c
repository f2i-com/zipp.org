/* Freestanding float32 kernels. No libc, Python runtime or external imports. */
__attribute__((export_name("binary")))
void binary(const float *a,const float *b,float *o,int count,int sa,int sb,int op) {
  for(int i=0;i<count;i++) {float x=a[sa?0:i],y=b[sb?0:i];o[i]=op==0?x+y:op==1?x-y:x*y;}
}
__attribute__((export_name("relu")))
void relu(const float *a,float *o,int count) {for(int i=0;i<count;i++)o[i]=a[i]>0?a[i]:0;}
__attribute__((export_name("fill")))
void fill(float *o,int count,float value) {for(int i=0;i<count;i++)o[i]=value;}
__attribute__((export_name("matmul")))
void matmul(const float*a,const float*b,float*o,int m,int k,int n) {
  for(int r=0;r<m;r++)for(int c=0;c<n;c++){float s=0;for(int j=0;j<k;j++)s+=a[r*k+j]*b[j*n+c];o[r*n+c]=s;}
}
__attribute__((export_name("pair_sum")))
void pair_sum(const float*a,float*o,int n){for(int i=0;i<(n+1)/2;i++)o[i]=a[2*i]+(2*i+1<n?a[2*i+1]:0);}
__attribute__((export_name("life")))
void life(const float*a,float*o,int h,int w){
 for(int y=0;y<h;y++)for(int x=0;x<w;x++){
  int count=0;for(int dy=-1;dy<=1;dy++)for(int dx=-1;dx<=1;dx++)
   if(dx||dy)count+=a[((y+dy+h)%h)*w+(x+dx+w)%w]>0.5f;
  o[y*w+x]=(count==3||(count==2&&a[y*w+x]>0.5f))?1:0;
 }
}

__attribute__((export_name("positive")))
void positive(const float*a,float*o,int n){for(int i=0;i<n;i++)o[i]=a[i]>0?1:0;}
__attribute__((export_name("transpose")))
void transpose(const float*a,float*o,int h,int w){for(int r=0;r<h;r++)for(int c=0;c<w;c++)o[c*h+r]=a[r*w+c];}
