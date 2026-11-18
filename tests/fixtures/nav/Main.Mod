MODULE Main;
  IMPORT L := Lib, Out;
  VAR head: L.Node; r: L.NodeDesc; k: INTEGER;

  PROCEDURE Show(n: L.Node);
    VAR i: INTEGER;
  BEGIN i := 0;
    WHILE n # NIL DO Out.Int(n.value); n := n.next; INC(i) END
  END Show;

BEGIN
  k := L.Add(head, L.Max);
  r.id := 1;
  Show(head)
END Main.
