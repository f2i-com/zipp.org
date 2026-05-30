let a=[];
for(let i=0;i<100000;i++) a.push(i);
let r=a.map(x=>x*2).filter(x=>x%3===0).reduce((p,c)=>p+c,0);
console.log(r);
