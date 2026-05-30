let s="";
for(let i=0;i<10000;i++){ s += (i%10); }
let c=0; for(let i=0;i<s.length;i++){ if(s[i]==="7") c++; }
console.log(s.length, c);
