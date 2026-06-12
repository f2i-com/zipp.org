let o={a:0,b:0,c:0};
let s=0;
for(let i=0;i<40000000;i++){ o.a=i; o.b=o.a+1; o.c=o.b*2; s+=o.c; }
console.log(s);
